export interface AppSource {
  path: string;
  anchor: string;
  digest?: string | null;
}
export interface AppNode {
  id: string;
  kind: string;
  title: string;
  description: string;
  symbol?: string | null;
  sources: AppSource[];
  excludes: string[];
}
export interface AppEdge {
  from: string;
  to: string;
  kind: string;
  view: "proof" | "flow";
}
export interface AppModel {
  version: number;
  title: string;
  description: string;
  author: string;
  exclusions: string[];
  nodes: AppNode[];
  edges: AppEdge[];
}
export interface AppSummary {
  id: string;
  title: string;
  description: string;
  guarantees: number;
  issues: string[];
}
export interface AppStage {
  name: string;
  status: string;
  seconds: number;
  log: string;
}
export interface AppTrial {
  name: string;
  generated: boolean;
  file: string;
  edits: { before: string; after: string }[];
  status: string;
  stages: AppStage[];
}
export interface Declaration {
  name: string;
  kind: string;
  signature: string;
  definition: string | null;
  dependencies: string[];
  axioms: string[];
}
export interface AppRun {
  id: string;
  kind: string;
  started: number;
  finished: number | null;
  status: string;
  inputs: {
    head: string | null;
    dirty: boolean;
    digest: string;
    errors: string[];
  };
  inputs_changed: boolean;
  targets: string[];
  toolchain: Record<string, string>;
  baseline: string;
  trials: AppTrial[];
  stages: AppStage[];
  declarations: Declaration[];
  unbuilt_modules: string[];
  error: string | null;
}
export interface RunView {
  run: AppRun;
  freshness: string;
}
export interface AppDetail {
  summary: AppSummary;
  model: AppModel | null;
  required_theorems: string[];
  files: string[];
  runs: RunView[];
  runs_total: number;
  head: string | null;
  issues: string[];
}
export interface AppChange {
  node: string;
  title: string;
  reason: string;
  path: string | null;
  before: string;
  after: string;
  affected: string[];
}
export interface AppRevision {
  commit: string;
  parent: string | null;
  time: number;
  message: string;
  changes: AppChange[];
}
export interface AppHistory {
  baseline: string;
  comparison: AppChange[];
  catalog_comparison: AppChange[];
  catalog_note: string;
  revisions: AppRevision[];
  scanned: number;
  truncated: boolean;
  shallow: boolean;
  notes: string[];
}
export interface SourceView {
  path: string;
  revision: string;
  text: string;
  start_line: number;
  total_lines: number;
  anchor_found: boolean;
}

async function read<T>(
  path: string,
  params?: Record<string, string>,
  signal?: AbortSignal,
): Promise<T> {
  const response = await fetch(
    path + (params ? `?${new URLSearchParams(params)}` : ""),
    { credentials: "same-origin", signal },
  );
  if (!response.ok) {
    const text = await response.text();
    let error: string;
    try {
      error = JSON.parse(text).error ?? text;
    } catch {
      error = text;
    }
    throw new Error(error || `Request failed (${response.status})`);
  }
  return response.json() as Promise<T>;
}
export const appsApi = {
  list: (signal?: AbortSignal) =>
    read<AppSummary[]>("/api/apps", undefined, signal),
  detail: (project: string, signal?: AbortSignal) =>
    read<AppDetail>("/api/apps/detail", { project }, signal),
  history: (project: string, baseline: string, signal?: AbortSignal) =>
    read<AppHistory>("/api/apps/changes", { project, baseline }, signal),
  source: (project: string, source: AppSource, signal?: AbortSignal) =>
    read<SourceView>(
      "/api/apps/source",
      { project, path: source.path, anchor: source.anchor },
      signal,
    ),
};

/** Edges point from a guarantee to the things it relies on. Keep this local
 * neighborhood stable while the inspector selection changes. Cycles terminate. */
export function neighborhood(
  model: AppModel,
  root: string,
  view: "proof" | "flow",
) {
  const edges = model.edges.filter((e) => e.view === view);
  const ids = new Set<string>(
    view === "flow" ? edges.flatMap((e) => [e.from, e.to]) : [root],
  );
  const queue = [root];
  while (view === "proof" && queue.length) {
    const id = queue.shift();
    for (const e of edges)
      if (e.from === id && !ids.has(e.to)) {
        ids.add(e.to);
        queue.push(e.to);
      }
  }
  return {
    nodes: model.nodes.filter((n) => ids.has(n.id)),
    edges: edges.filter((e) => ids.has(e.from) && ids.has(e.to)),
  };
}

export const labels: Record<string, string> = {
  proof_failed: "Proof-stage failure",
  survived: "Survived",
  invalid: "Invalid",
  error: "Execution error",
  running: "Running / unfinished",
  pending: "Pending",
  skipped: "Skipped",
  passed: "Passed",
  failed: "Failed",
  interrupted: "Interrupted",
  completed: "Completed",
  completed_with_issues: "Needs review",
  not_run: "Not run",
  matches_repository_inputs: "Repository inputs match",
  stale: "Inputs have changed",
  unknown: "Freshness unknown",
  changed_during_run: "Inputs changed during run",
  lean_build: "Lean build",
  rust_check: "Rust check",
  axiom_gate: "Axiom gate",
  extraction: "Extraction",
  sandbox: "Prepare copy",
  edit: "Apply edit",
  packages: "Packages",
};
export const label = (s: string) => labels[s] ?? s;
export const shellQuote = (s: string) => `'${s.replace(/'/g, `'"'"'`)}'`;
