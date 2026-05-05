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

export interface BranchInfo {
  name: string;
  is_head: boolean;
  is_remote: boolean;
  upstream: string | null;
  target_oid: string | null;
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

async function getJSON<T>(url: string): Promise<T> {
  const res = await fetch(url);
  if (!res.ok) throw new Error(`${res.status} ${res.statusText} — ${url}`);
  return res.json() as Promise<T>;
}

export const api = {
  repo: () => getJSON<Repo>("/api/repo"),
  branches: () => getJSON<BranchInfo[]>("/api/branches"),
  commits: (opts: { limit?: number; branch?: string | null } = {}) => {
    const p = new URLSearchParams();
    if (opts.limit != null) p.set("limit", String(opts.limit));
    if (opts.branch) p.set("branch", opts.branch);
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
  contextRelevant: (file: string) =>
    getJSON<ContextRelevant | unknown>(
      `/api/context/relevant?file=${encodeURIComponent(file)}`,
    ),

  // Workbench-mode views
  memorySnapshots: () => getJSON<MemorySnapshot[]>("/api/memory/snapshots"),
  reviewPoints: (limit = 100, minScore = 0.25) =>
    getJSON<ReviewPoint[]>(
      `/api/review-points?limit=${limit}&min_score=${minScore}`,
    ),
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
