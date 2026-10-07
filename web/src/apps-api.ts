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
  origin?: "lean" | "authored";
}
export interface AppEdge {
  from: string;
  to: string;
  kind: string;
  view: "proof" | "flow";
  origin?: "lean" | "authored";
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
  anchor_line: number | null;
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

/** Lean-to-Lean dependency edges come exclusively from the selected catalog.
 * Authored nodes keep their explanations and source anchors. Uncataloged
 * dependencies are explicit leaves, not invented declarations. */
export function withLeanDependencies(
  authored: AppModel | null,
  declarations: Declaration[],
): AppModel {
  const model: AppModel = authored ?? {
    version: 1,
    title: "Lean declarations",
    description: "",
    author: "",
    exclusions: [],
    nodes: [],
    edges: [],
  };
  const nodes = model.nodes.map(
    (n) => ({ ...n, origin: "authored" as const }) as AppNode,
  );
  const isLean = (n: AppNode) =>
    !!n.symbol &&
    ["theorem", "specification", "assumption", "counterexample"].includes(
      n.kind,
    );
  const authoredLean = new Set(nodes.filter(isLean).map((n) => n.id));
  const symbols = new Map<string, AppNode[]>();
  for (const n of nodes.filter(isLean))
    symbols.set(n.symbol!, [...(symbols.get(n.symbol!) ?? []), n]);
  const ids = new Set(nodes.map((n) => n.id));
  const catalog = new Map(declarations.map((d) => [d.name, d]));
  const ensure = (name: string) => {
    if (symbols.has(name)) return symbols.get(name)!;
    const d = catalog.get(name);
    let id = `lean:${name}`;
    while (ids.has(id)) id = `_${id}`;
    ids.add(id);
    const node: AppNode = {
      id,
      symbol: name,
      origin: "lean",
      kind: d?.kind === "theorem" ? "theorem" : "specification",
      title: name.split(".").slice(-2).join("."),
      description: d
        ? "Declaration recorded from the Lean environment. Select a connection to follow its direct dependencies."
        : "Referenced Lean constant outside this catalog. Its type and further dependencies were not recorded.",
      sources: [],
      excludes: [],
    };
    nodes.push(node);
    symbols.set(name, [node]);
    return [node];
  };
  for (const d of declarations) ensure(d.name);
  const edges: AppEdge[] = model.edges
    .filter(
      (e) =>
        e.view !== "proof" ||
        !authoredLean.has(e.from) ||
        !authoredLean.has(e.to),
    )
    .map((e) => ({ ...e, origin: "authored" }));
  for (const d of declarations) {
    for (const dependency of new Set(d.dependencies)) {
      if (dependency === d.name) continue;
      const to = ensure(dependency)[0].id;
      for (const from of ensure(d.name))
        edges.push({
          from: from.id,
          to,
          kind: "depends on",
          view: "proof",
          origin: "lean",
        });
    }
  }
  return { ...model, nodes, edges };
}

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
  const outgoing = new Map<string, AppEdge[]>();
  for (const e of edges)
    outgoing.set(e.from, [...(outgoing.get(e.from) ?? []), e]);
  while (view === "proof" && queue.length) {
    const id = queue.shift();
    for (const e of outgoing.get(id!) ?? [])
      if (!ids.has(e.to)) {
        ids.add(e.to);
        queue.push(e.to);
      }
  }
  const visible = new Set([...ids].slice(0, 80));
  return {
    nodes: model.nodes.filter((n) => visible.has(n.id)),
    edges: edges.filter((e) => visible.has(e.from) && visible.has(e.to)),
    total: ids.size,
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

export interface Drift {
  /** Authored nodes that cite at least one source. */
  cited: number;
  /** Of those, nodes whose every source carries a fingerprint. */
  fingerprinted: number;
  /** Node title → why its explanation may no longer match its source. */
  drifted: Map<string, string[]>;
}

const ANCHOR_ISSUE = ": source anchor missing or ambiguous in ";
const DIGEST_ISSUE = ": explanation's source fingerprint is stale";

/** How far an authored map may have fallen behind the code, from the
 *  anchor and fingerprint issues the server already reports. */
export function explanationDrift(
  model: AppModel | null,
  issues: string[],
): Drift {
  const nodes = (model?.nodes ?? []).filter((n) => n.sources.length);
  const titles = new Set(nodes.map((n) => n.title));
  const drifted = new Map<string, string[]>();
  const add = (title: string, why: string) => {
    if (!titles.has(title)) return;
    drifted.set(title, [...(drifted.get(title) ?? []), why]);
  };
  for (const issue of issues) {
    const a = issue.indexOf(ANCHOR_ISSUE);
    if (a > 0)
      add(
        issue.slice(0, a),
        `anchor no longer found once in ${issue.slice(a + ANCHOR_ISSUE.length)}`,
      );
    else if (issue.endsWith(DIGEST_ISSUE))
      add(
        issue.slice(0, -DIGEST_ISSUE.length),
        "a cited file changed since this explanation was reviewed",
      );
  }
  return {
    cited: nodes.length,
    fingerprinted: nodes.filter((n) => n.sources.every((s) => s.digest)).length,
    drifted,
  };
}

/** Columns for the flow lens: each node one step right of what feeds it, so
 *  a request reads left to right. Edges that close a cycle (a reply back to
 *  the route) are ignored for depth. */
export function flowLayers(nodes: AppNode[], edges: AppEdge[]): string[][] {
  const ids = nodes.map((n) => n.id);
  const known = new Set(ids);
  const preds = new Map<string, string[]>(ids.map((id) => [id, []]));
  const succs = new Map<string, string[]>(ids.map((id) => [id, []]));
  for (const e of edges)
    if (known.has(e.from) && known.has(e.to) && e.from !== e.to) {
      preds.get(e.to)!.push(e.from);
      succs.get(e.from)!.push(e.to);
    }
  // Depth-first from the entry points; an edge back onto the current path is
  // a cycle and does not push its target further right.
  const depth = new Map<string, number>();
  const onPath = new Set<string>();
  const visit = (id: string, d: number) => {
    if (onPath.has(id) || (depth.get(id) ?? -1) >= d) return;
    depth.set(id, d);
    onPath.add(id);
    for (const s of succs.get(id)!) visit(s, d + 1);
    onPath.delete(id);
  };
  const entries = ids.filter((id) => preds.get(id)!.length === 0);
  // A flow that loops back everywhere starts where it mostly sends.
  const lead = (id: string) => succs.get(id)!.length - preds.get(id)!.length;
  const start = entries.length
    ? entries
    : ids
        .slice()
        .sort((a, b) => lead(b) - lead(a))
        .slice(0, 1);
  for (const id of start) visit(id, 0);
  for (const id of ids) if (!depth.has(id)) visit(id, 0);
  const layers: string[][] = [];
  for (const id of ids) (layers[depth.get(id)!] ??= []).push(id);
  // One barycenter pass: sit each node level with what feeds it.
  const row = new Map<string, number>();
  return layers.filter(Boolean).map((layer) => {
    const at = (id: string) => {
      const p = preds.get(id)!.filter((x) => row.has(x));
      return p.length
        ? p.reduce((n, x) => n + row.get(x)!, 0) / p.length
        : Infinity;
    };
    const sorted = layer
      .map((id, i) => ({ id, i, y: at(id) }))
      .sort((a, b) => a.y - b.y || a.i - b.i)
      .map((x) => x.id);
    sorted.forEach((id, i) => row.set(id, i));
    return sorted;
  });
}
