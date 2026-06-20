// Thin client over the existing axum endpoints exposed by `h5i serve`.
// Types mirror what server.rs returns (kept hand-written to avoid an extra
// codegen step while the surface is still small).

export interface Repo {
  name: string;
  branch: string;
  total_commits: number;
  ai_commits: number;
  tested_commits: number;
  test_pass_rate: number | null;
  github_url?: string | null;
}

export interface Commit {
  git_oid: string;
  short_oid: string;
  message: string;
  author: string;
  timestamp: string;
  ai_model: string | null;
  ai_agent: string | null;
  ai_prompt: string | null;
  ai_tokens: number | null;
  test_coverage: number | null;
  test_passed: number | null;
  test_failed: number | null;
  test_skipped: number | null;
  test_total: number | null;
  test_duration_secs: number | null;
  test_tool: string | null;
  test_exit_code: number | null;
  test_is_passing: boolean | null;
  ast_file_count: number | null;
  has_crdt: boolean;
}

export interface IntentNode {
  oid: string;
  short_oid: string;
  message: string;
  intent: string;
  intent_source: string;
  author: string;
  timestamp: string;
  is_ai: boolean;
  agent: string | null;
  model: string | null;
}
export interface IntentEdge {
  from: string;
  to: string;
  kind: string; // "parent" | "causal"
}
export interface IntentGraph {
  nodes: IntentNode[];
  edges: IntentEdge[];
}

export interface SessionLogMeta {
  commit_oid: string;
  session_id: string;
  analyzed_at: string;
  message_count: number;
  tool_call_count: number;
  edited_count: number;
  consulted_count: number;
  uncertainty_count: number;
}

export interface IntegrityFinding {
  rule_id: string;
  severity: "Valid" | "Warning" | "Violation" | string;
  detail: string;
}
export interface IntegrityReport {
  level: "Valid" | "Warning" | "Violation" | string;
  score: number;
  findings: IntegrityFinding[];
}

export interface ContextBranchSummary {
  branch: string;
  purpose: string;
  last_milestone: string;
  last_activity: string;
  todo_count: number;
  trace_lines: number;
  milestone_count: number;
  snapshot_count: number;
  exclusive_milestones: number;
  exclusive_trace_lines: number;
  is_scope: boolean;
}
export interface ContextStatus {
  initialized: boolean;
  current_branch: string;
  git_branch: string;
  git_branch_goal: string;
  goal: string;
  branch_count: number;
  branches: string[];
  commit_count: number;
  trace_lines: number;
  snapshot_count: number;
  stable_line_count: number;
  dynamic_line_count: number;
  todo_count: number;
  latest_snapshot_timestamp: string;
  stale_branch_count: number;
  branch_summaries: ContextBranchSummary[];
}

// `/api/context/show` — what `h5i context show` returns. The richest single
// endpoint: full milestone list, recent commits, mini OTA trace, todos.
export interface ContextShow {
  project_goal: string;
  git_branch: string;
  git_branch_goal: string;
  milestones: string[];
  active_branches: string[];
  current_branch: string;
  recent_commits: string[];
  recent_log_lines: string[];
  metadata_snippet: string | null;
  stable_line_count: number;
  dynamic_line_count: number;
  todo_items: string[];
  mini_trace: string[];
}

// `/api/context/promotion` — promotion pipeline counts for a single branch.
export interface ContextPromotion {
  branch: string;
  purpose: string;
  ephemeral_count: number;
  durable_trace_count: number;
  milestone_count: number;
  snapshot_count: number;
  todo_count: number;
  stable_line_count: number;
  dynamic_line_count: number;
  last_snapshot_timestamp: string;
  recent_milestones: string[];
}

// `/api/context/dag` — OTA-graph node counts plus the actual node list.
export interface ContextDagNode {
  id: string;
  parent_ids: string[];
  kind: "OBSERVE" | "THINK" | "ACT" | "NOTE" | "MERGE" | string;
  content: string;
  timestamp: string;
}
export interface ContextDag {
  branch: string;
  node_count: number;
  observe_count: number;
  think_count: number;
  act_count: number;
  note_count: number;
  merge_count: number;
  nodes: ContextDagNode[];
}

// `/api/context/snapshots` — context snapshots tied to git commits.
export interface ContextSnapshotItem {
  sha: string;
  sha_short: string;
  context_oid: string;
  timestamp: string;
  branch: string;
  goal: string;
  recent_milestones: string[];
}

// `/api/context/milestones?branch=X` — structured per-commit milestones with
// the context-commit short SHA and timestamp from the commit.md header.
// Lets the UI show a hash chip per milestone instead of just text.
export interface ContextMilestoneEntry {
  sha_short: string;
  timestamp: string;
  contribution: string;
}

// `/api/context/diff?from=&to=` — the delta between two context snapshots.
export interface ContextDiff {
  from: string;
  to: string;
  from_branch: string;
  to_branch: string;
  cross_branch: boolean;
  goal_changed: boolean;
  from_goal: string;
  to_goal: string;
  added_milestones: string[];
  removed_milestones: string[];
  added_trace_lines: string[];
  removed_trace_lines: string[];
}

export interface MemorySnapshot {
  oid: string;
  short_oid: string;
  message: string;
  author: string;
  timestamp: string;
  file_count: number;
  total_bytes: number;
}

export interface ReviewTrigger {
  rule_id: string;
  weight: number;
  detail: string;
}
export interface ReviewPoint {
  commit_oid: string;
  short_oid: string;
  message: string;
  author: string;
  timestamp: string;
  score: number;
  triggers: ReviewTrigger[];
}

export interface BranchLastCommit {
  oid: string;
  short_oid: string;
  message: string;
  author: string;
  timestamp: string;
}

export interface ContextBranchLink {
  name: string;
  purpose: string;
  last_milestone: string;
  last_activity: string;
  milestone_count: number;
  trace_lines: number;
  snapshot_count: number;
  todo_count: number;
}

export interface BranchInfo {
  name: string;
  is_head: boolean;
  is_remote: boolean;
  upstream: string | null;
  target_oid: string | null;
  /** Commits ahead of upstream (null when no upstream tracking). */
  ahead: number | null;
  /** Commits behind upstream (null when no upstream tracking). */
  behind: number | null;
  /** Tip of the branch — most recent commit. */
  last_commit: BranchLastCommit | null;
  /** AI-assisted commits within the walked window. */
  ai_commit_count: number | null;
  /** How many commits we walked from the branch tip (capped). */
  walked_commit_count: number | null;
  /** Same-named context branch info, when one exists. */
  context: ContextBranchLink | null;
  /** Whether a same-named context branch exists. Drives the "Create context" CTA. */
  has_context_branch: boolean;
}

export interface CommitFiles {
  oid: string;
  files: string[];
  truncated: boolean;
}

// `/api/context/relevant` returns one of these shapes (see ctx::find_relevant):
// either a structured envelope or a plain string array, depending on whether
// any matches were found. We accept both and normalise in the consumer.
export interface ContextRelevantHit {
  branch: string;
  source: string; // "milestone" | "trace" | etc.
  text: string;
}
export interface ContextRelevant {
  file: string;
  hits: ContextRelevantHit[];
}

// ── Sandbox dashboard (the "flight recorder") ───────────────────────────────
// Mirror of the read-only env-monitoring endpoints in server.rs.

export type RiskLane = "fs" | "net" | "proc" | "resource" | "provenance";
export type RiskSeverity = "info" | "warning" | "critical";

export interface RiskFinding {
  severity: RiskSeverity;
  lane: RiskLane;
  kind: string;
  title: string; // "Boundary blocked" | "Boundary pressure" | "Weak isolation" | …
  detail: string;
  evidence: string;
  capture_id?: string | null;
  event_ts?: string | null;
}

export interface EnvRisk {
  score: number; // 0..=100
  level: RiskSeverity;
  findings: RiskFinding[];
  lane_counts: Record<string, number>;
  last_denial_ts?: string | null;
}

export interface EnvEventView {
  ts: string;
  event: string; // created | exec | status | proposed | applied | aborted | gc | violation
  detail?: string | null;
  capture?: string | null;
}

export interface EnvFleetItem {
  id: string;
  agent: string;
  slug: string;
  status: string;
  isolation: string;
  profile: string;
  backend: string;
  policy_digest: string;
  parent_branch: string;
  created_at: string;
  updated_at: string;
  captures: number;
  has_workspace: boolean;
  drift: "up-to-date" | "parent-ahead" | "diverged" | "parent-gone" | string;
  drift_summary: string;
  last_event?: EnvEventView | null;
  risk: EnvRisk;
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
  hosts: EgressHost[];
  hosts_truncated: boolean;
  log?: string | null;
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
  image?: string | null;
  mem_bytes?: number | null;
  max_procs?: number | null;
  wall_secs: number;
  cpu_secs?: number | null;
  fsize_bytes?: number | null;
}

export interface EnvCaptureView {
  id: string;
  cmd?: string | null;
  exit_code?: number | null;
  timestamp: string;
  summary: string;
  egress?: EgressSummary | null;
  redactions: string[];
}

export interface EnvDetail {
  item: EnvFleetItem;
  policy?: EnforcedPolicy | null;
  events: EnvEventView[];
  captures: EnvCaptureView[];
  diffstat?: string | null;
}

export interface ProbeTier {
  claim: string;
  satisfiable: boolean;
  note?: string | null;
}
export interface CgroupProbe {
  v2_mounted: boolean;
  usable: boolean;
  controllers: string[];
  detail?: string | null;
}
export interface SupervisorComponent {
  name: string;
  ok: boolean;
  detail?: string | null;
}
export interface SupervisorProbe {
  usable: boolean;
  components: SupervisorComponent[];
}
export interface ProbeResponse {
  os: string;
  landlock_abi?: number | null;
  userns: boolean;
  seccomp: boolean;
  container_runtime?: string | null;
  tiers: ProbeTier[];
  process_runnable: boolean;
  process_runnable_detail?: string | null;
  cgroups: CgroupProbe;
  supervisor: SupervisorProbe;
}

// ── Replay (the flight recorder) ────────────────────────────────────────────
// Mirror of the unified replay endpoints in server.rs. One shape for both the
// env anchor (/api/env/:a/:s/replay) and the commit fallback
// (/api/commit/:oid/replay).

export type ReplayKind =
  | "PROMPT" | "THINK" | "READ" | "RUN" | "TEST_PASS" | "TEST_FAIL"
  | "BLOCKED" | "EDIT" | "NOTE" | "DIFF" | "CREATE" | "PROPOSE"
  | "APPLY" | "ABORT" | "MSG" | "EVENT" | string;

export type ReplayLane =
  | "intent" | "fs" | "net" | "proc" | "test" | "provenance" | "lifecycle" | "msg" | string;

export type ReplaySeverity = "info" | "good" | "warning" | "critical";

export interface ReplayEvent {
  seq: number;
  ts: string;
  kind: ReplayKind;
  lane: ReplayLane;
  title: string;
  detail?: string | null;
  severity: ReplaySeverity;
  files?: string[];
  capture_id?: string | null;
  exit_code?: number | null;
}

export interface FileHeat {
  path: string;
  read: boolean;
  edited: boolean;
  tested: boolean;
  blocked: boolean;
  risky: boolean;
}

export interface ReplayHeader {
  anchor: "env" | "commit" | string;
  id: string;
  title: string;
  subtitle?: string | null;
  agent?: string | null;
  model?: string | null;
  isolation?: string | null;
  prompt?: string | null;
  policy_digest?: string | null;
  blocked_count: number;
  allowed_count: number;
  tests_passed?: number | null;
  tests_failed?: number | null;
  risk_score: number;
  risk_level: string;
  run_count: number;
  created_at?: string | null;
  diffstat?: string | null;
}

export interface ReplayView {
  header: ReplayHeader;
  timeline: ReplayEvent[];
  heatmap: FileHeat[];
  policy?: EnforcedPolicy | null;
  findings: RiskFinding[];
}

// ── Reviewer cockpit ────────────────────────────────────────────────────────

export interface CockpitFile {
  path: string;
  reason: string;
  severity: string;
}
export interface ReviewerCockpit {
  oid: string;
  short_oid: string;
  message: string;
  author: string;
  timestamp: string;
  merge_confidence: number;
  prompt_maturity?: number | null;
  provenance: string;
  model?: string | null;
  sandbox?: string | null;
  policy_digest?: string | null;
  net_blocked: number;
  net_allowed: number;
  tests_passed?: number | null;
  tests_failed?: number | null;
  integrity_level: string;
  integrity_score: number;
  risk: "low" | "medium" | "high" | string;
  review_first: CockpitFile[];
  review_score: number;
}

// ── Prompt-maturity coach ───────────────────────────────────────────────────

export interface PromptMaturity {
  prompt: string;
  score: number;
  level: string;
  words: number;
  flags: string[];
  suggested_upgrade?: string | null;
}

// ── Agent radio (review threads, not chat) ──────────────────────────────────

export interface RadioMessage {
  id: string;
  ts: string;
  from: string;
  to: string;
  kind: string;
  body: string;
  status?: string | null;
  priority?: string | null;
  branch?: string | null;
  focus?: string[];
  risk?: string | null;
}
export interface RadioThread {
  thread_id: string;
  latest_ts: string;
  branch?: string | null;
  status: string;
  messages: RadioMessage[];
}

async function getJSON<T>(url: string): Promise<T> {
  const res = await fetch(url);
  if (!res.ok) throw new Error(`${res.status} ${res.statusText} — ${url}`);
  return res.json() as Promise<T>;
}

export const api = {
  repo: () => getJSON<Repo>("/api/repo"),
  branches: () => getJSON<BranchInfo[]>("/api/branches"),
  commits: (
    opts: { limit?: number; branch?: string | null; branchOnly?: boolean } = {},
  ) => {
    const p = new URLSearchParams();
    if (opts.limit != null) p.set("limit", String(opts.limit));
    if (opts.branch) p.set("branch", opts.branch);
    if (opts.branchOnly) p.set("branch_only", "true");
    return getJSON<Commit[]>(`/api/commits?${p.toString()}`);
  },
  commitFiles: (oid: string) =>
    getJSON<CommitFiles>(`/api/commit-files?oid=${encodeURIComponent(oid)}`),

  // Cross-reference / right-pane data
  intentGraph: (limit = 60, mode: "prompt" | "analyze" = "prompt") =>
    getJSON<IntentGraph>(
      `/api/intent-graph?limit=${limit}&mode=${mode}`,
    ),
  sessionList: () => getJSON<SessionLogMeta[]>("/api/session-log/list"),
  integrityCommit: (oid: string) =>
    getJSON<IntegrityReport>(
      `/api/integrity/commit?oid=${encodeURIComponent(oid)}`,
    ),
  contextStatus: () => getJSON<ContextStatus>("/api/context/status"),
  contextShow: () => getJSON<ContextShow>("/api/context/show"),
  contextPromotion: () => getJSON<ContextPromotion>("/api/context/promotion"),
  contextDag: () => getJSON<ContextDag>("/api/context/dag"),
  contextSnapshots: () => getJSON<ContextSnapshotItem[]>("/api/context/snapshots"),
  contextDiff: (from: string, to: string) =>
    getJSON<ContextDiff>(
      `/api/context/diff?from=${encodeURIComponent(from)}&to=${encodeURIComponent(to)}`,
    ),
  contextMilestones: (branch?: string) => {
    const q = branch ? `?branch=${encodeURIComponent(branch)}` : "";
    return getJSON<ContextMilestoneEntry[]>(`/api/context/milestones${q}`);
  },
  contextRelevant: (file: string) =>
    getJSON<ContextRelevant | unknown>(
      `/api/context/relevant?file=${encodeURIComponent(file)}`,
    ),

  // Workbench-mode views
  memorySnapshots: () => getJSON<MemorySnapshot[]>("/api/memory/snapshots"),
  reviewPoints: (limit = 100, minScore = 0.25, branch?: string | null) => {
    const p = new URLSearchParams();
    p.set("limit", String(limit));
    p.set("min_score", String(minScore));
    if (branch) p.set("branch", branch);
    return getJSON<ReviewPoint[]>(`/api/review-points?${p.toString()}`);
  },

  // Sandbox dashboard
  envs: () => getJSON<EnvFleetItem[]>("/api/envs"),
  envProbe: () => getJSON<ProbeResponse>("/api/env/probe"),
  envDetail: (agent: string, slug: string) =>
    getJSON<EnvDetail>(
      `/api/env/${encodeURIComponent(agent)}/${encodeURIComponent(slug)}`,
    ),
  envCapture: (agent: string, slug: string, id: string) =>
    getJSON<{ render: string }>(
      `/api/env/${encodeURIComponent(agent)}/${encodeURIComponent(
        slug,
      )}/captures/${encodeURIComponent(id)}`,
    ),

  // Replay (the flight recorder) + cockpit + prompt coach + radio
  envReplay: (agent: string, slug: string) =>
    getJSON<ReplayView>(
      `/api/env/${encodeURIComponent(agent)}/${encodeURIComponent(slug)}/replay`,
    ),
  commitReplay: (oid: string) =>
    getJSON<ReplayView>(`/api/commit/${encodeURIComponent(oid)}/replay`),
  cockpit: (oid: string) =>
    getJSON<ReviewerCockpit>(`/api/cockpit?oid=${encodeURIComponent(oid)}`),
  promptScore: (opts: { oid?: string; text?: string }) => {
    const p = new URLSearchParams();
    if (opts.oid) p.set("oid", opts.oid);
    if (opts.text) p.set("text", opts.text);
    return getJSON<PromptMaturity | null>(`/api/prompt-score?${p.toString()}`);
  },
  radio: () => getJSON<RadioThread[]>("/api/radio"),
};

// ── GitHub URL helpers ─────────────────────────────────────────────────────────
//
// Repo.github_url is the cleaned-up "https://github.com/<owner>/<repo>" string
// already extracted from the `origin` remote on the Rust side. We just need to
// append the right path for commits / branches / files.

export function githubCommitUrl(github_url: string | null | undefined, oid: string): string | null {
  if (!github_url || !oid) return null;
  return `${github_url.replace(/\/$/, "")}/commit/${oid}`;
}

export function githubBranchUrl(
  github_url: string | null | undefined,
  branch: string,
): string | null {
  if (!github_url || !branch) return null;
  // "origin/main" → "main"; remote-only branches still resolve via the same path.
  const clean = branch.replace(/^origin\//, "");
  return `${github_url.replace(/\/$/, "")}/tree/${encodeURIComponent(clean)}`;
}

export function githubFileUrl(
  github_url: string | null | undefined,
  branch: string,
  path: string,
): string | null {
  if (!github_url || !path) return null;
  const clean = (branch || "HEAD").replace(/^origin\//, "");
  return `${github_url.replace(/\/$/, "")}/blob/${encodeURIComponent(clean)}/${path
    .split("/")
    .map(encodeURIComponent)
    .join("/")}`;
}
