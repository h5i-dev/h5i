import { useEffect, useMemo, useState } from "react";
import {
  appsApi,
  label,
  neighborhood,
  withLeanDependencies,
  shellQuote,
  type AppChange,
  type AppDetail,
  type AppHistory,
  type AppModel,
  type AppSource,
  type AppStage,
  type AppSummary,
  type RunView,
  type SourceView,
} from "./apps-api";
import { Chip, Cmd, Empty, Note, Spin, Split } from "./ui";
import "./apps.css";

type Go = (parts: string[], replace?: boolean) => void;
const date = (seconds: number) => new Date(seconds * 1000).toLocaleString();
const short = (hash?: string | null) => hash?.slice(0, 10) ?? "no revision";
const warn = (s: string) =>
  [
    "failed",
    "survived",
    "invalid",
    "error",
    "interrupted",
    "stale",
    "changed_during_run",
    "completed_with_issues",
  ].includes(s);

export function AppsPage({ route, go }: { route: string[]; go: Go }) {
  const [apps, setApps] = useState<AppSummary[] | null>(null);
  const [error, setError] = useState("");
  const [search, setSearch] = useState("");
  const [refresh, setRefresh] = useState(0);
  useEffect(() => {
    const controller = new AbortController();
    appsApi
      .list(controller.signal)
      .then((v) => {
        setApps(v);
        setError("");
      })
      .catch((e) => {
        if (!controller.signal.aborted) setError(String(e));
      });
    return () => controller.abort();
  }, [refresh]);
  const filtered = apps?.filter((a) =>
    `${a.title} ${a.id} ${a.description}`
      .toLowerCase()
      .includes(search.toLowerCase()),
  );
  return (
    <div className="apps-page">
      <header className="topbar">
        <span className="topbar-title">Apps</span>
        <span className="topbar-scope">
          Behavior, conditions, and the evidence behind them
        </span>
        <button className="app-button" onClick={() => setRefresh((n) => n + 1)}>
          Refresh
        </button>
      </header>
      <Split
        id="app-projects"
        initial={260}
        min={210}
        max={420}
        first={
          <aside className="app-projects">
            <div className="app-search">
              <input
                aria-label="Find an app"
                placeholder="Find an app…"
                value={search}
                onChange={(e) => setSearch(e.target.value)}
              />
            </div>
            <div className="app-project-list">
              {error && <Note tone="warn">{error}</Note>}
              {!apps && !error && <Spin label="Reading app projects" />}
              {filtered?.map((a) => (
                <button
                  className={`app-project ${route[0] === a.id ? "is-on" : ""}`}
                  key={a.id}
                  onClick={() => go(["apps", a.id, "guarantees"])}
                  aria-current={route[0] === a.id ? "page" : undefined}
                >
                  <span className="app-project-title">
                    {a.title}
                    <span className="app-count">{a.guarantees || "—"}</span>
                  </span>
                  <span className="app-project-description">
                    {a.description}
                  </span>
                  <code>{a.id}</code>
                  {!!a.issues.length && (
                    <span className="app-warning">
                      Explanation needs attention
                    </span>
                  )}
                </button>
              ))}
              {apps && !filtered?.length && (
                <p className="app-muted app-pad">
                  {apps.length
                    ? "No apps match this search."
                    : "No h5i-app.toml projects found."}
                </p>
              )}
            </div>
            <footer className="app-project-footer">
              {apps?.length ?? "—"} projects · this repository
            </footer>
          </aside>
        }
        second={
          route[0] ? (
            <AppWorkspace
              key={route[0]}
              project={route[0]}
              route={route.slice(1)}
              refresh={refresh}
              go={go}
            />
          ) : (
            <div className="app-welcome">
              <span className="app-eyebrow">Application review</span>
              <h1>What does your app promise?</h1>
              <p>
                Follow a guarantee from its specification to the code, the
                conditions it relies on, and the bugs its proofs detect.
              </p>
              <div className="app-welcome-grid">
                <div>
                  <b>01 / Understand</b>
                  <p>Explore guarantees and their boundaries.</p>
                </div>
                <div>
                  <b>02 / Compare</b>
                  <p>Review changes to conditions and dependencies.</p>
                </div>
                <div>
                  <b>03 / Challenge</b>
                  <p>Inspect mutation experiments and surviving changes.</p>
                </div>
              </div>
              {apps?.length === 0 ? (
                <Cmd text="h5i app new my-app" />
              ) : (
                <p className="app-muted">Select a project to begin.</p>
              )}
            </div>
          )
        }
      />
    </div>
  );
}

function AppWorkspace({
  project,
  route,
  refresh,
  go,
}: {
  project: string;
  route: string[];
  refresh: number;
  go: Go;
}) {
  const [data, setData] = useState<AppDetail | null>(null);
  const [error, setError] = useState("");
  const [tick, setTick] = useState(0);
  useEffect(() => {
    const t = window.setInterval(() => setTick((n) => n + 1), 30000);
    return () => window.clearInterval(t);
  }, []);
  useEffect(() => {
    const controller = new AbortController();
    appsApi
      .detail(project, controller.signal)
      .then((v) => {
        setData(v);
        setError("");
      })
      .catch((e) => {
        if (!controller.signal.aborted) setError(String(e));
      });
    return () => controller.abort();
  }, [project, refresh, tick]);
  const tab = ["guarantees", "changes", "mutations"].includes(route[0])
    ? route[0]
    : "guarantees";
  if (!data)
    return error ? (
      <Empty title="Could not read this app">
        <p>{error}</p>
      </Empty>
    ) : (
      <Spin label="Reading app evidence" />
    );
  const latest = data.runs.find((r) => r.run.kind === "check");
  return (
    <section className="app-workspace">
      <header className="app-heading">
        <div className="app-eyebrow">Application / {project}</div>
        <div className="app-title-row">
          <h1>{data.summary.title}</h1>
          <Chip tone={latest && warn(latest.run.status) ? "warn" : "dim"}>
            {latest
              ? `Last check: ${label(latest.run.status)}`
              : "No check recorded"}
          </Chip>
        </div>
        <p>{data.summary.description}</p>
        <div className="app-meta">
          <code>{short(data.head)}</code>
          <span>Working tree</span>
          {latest && <span>{label(latest.freshness)}</span>}
        </div>
      </header>
      <nav className="tabs" aria-label="App views">
        {["guarantees", "changes", "mutations"].map((t) => (
          <button
            key={t}
            className={`tab ${tab === t ? "is-on" : ""}`}
            aria-current={tab === t ? "page" : undefined}
            onClick={() => go(["apps", project, t])}
          >
            {t[0].toUpperCase() + t.slice(1)}
          </button>
        ))}
      </nav>
      {error && (
        <Note tone="warn">
          Refresh failed; showing the last loaded data. {error}
        </Note>
      )}
      {!!data.issues.length && (
        <details className="app-issues">
          <summary>
            {data.issues.length} source or record issue
            {data.issues.length === 1 ? "" : "s"}
          </summary>
          {data.issues.map((s, i) => (
            <p key={i}>{s}</p>
          ))}
        </details>
      )}
      <div className="app-content">
        {tab === "guarantees" ? (
          <Guarantees data={data} route={route.slice(1)} go={go} />
        ) : tab === "changes" ? (
          <Changes project={project} refresh={refresh} />
        ) : (
          <Mutations data={data} />
        )}
      </div>
    </section>
  );
}

function Guarantees({
  data,
  route,
  go,
}: {
  data: AppDetail;
  route: string[];
  go: Go;
}) {
  const latest = data.runs.find((v) => v.run.kind === "check");
  const model = useMemo(
    () => withLeanDependencies(data.model, latest?.run.declarations ?? []),
    [data.model, latest],
  );
  const [filter, setFilter] = useState("");
  const [view, setView] = useState<"proof" | "flow">("proof");
  const [file, setFile] = useState<AppSource | null>(null);
  const authoredGuarantees = model.nodes.filter((n) => n.kind === "guarantee");
  const catalogNames = new Set(
    latest?.run.declarations.map((d) => d.name) ?? [],
  );
  const guarantees = authoredGuarantees.length
    ? authoredGuarantees
    : model.nodes.filter((n) => n.symbol && catalogNames.has(n.symbol));
  const root = model.nodes.find((n) => n.id === route[0]) ?? guarantees[0];
  const selected = model?.nodes.find((n) => n.id === route[1]) ?? root;
  const select = (id: string) => {
    setFile(null);
    go(["apps", data.summary.id, "guarantees", root?.id ?? id, id]);
  };
  if (!model || !root)
    return (
      <div className="app-pad app-scroll">
        <MapPrompt project={data.summary.id} />
        <Evidence view={latest} project={data.summary.id} />
      </div>
    );
  const related = model.edges.filter(
    (e) => e.from === selected?.id || e.to === selected?.id,
  );
  const declaration = selected?.symbol
    ? latest?.run.declarations.find((d) => d.name === selected.symbol)
    : undefined;
  return (
    <div className="app-guarantees">
      <div className="app-guarantee-list">
        <div className="app-section-heading">
          <span className="app-eyebrow">
            {authoredGuarantees.length
              ? "Promises & conditions"
              : "Recorded Lean declarations"}
          </span>
          <input
            aria-label="Search guarantees or theorems"
            placeholder="Search guarantees or theorems…"
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
          />
        </div>
        <div className="app-cards">
          {guarantees
            .filter((n) =>
              `${n.title} ${n.symbol ?? ""} ${n.description}`
                .toLowerCase()
                .includes(filter.toLowerCase()),
            )
            .slice(0, 40)
            .map((n, i) => (
              <button
                key={n.id}
                className={`app-guarantee ${root.id === n.id ? "is-on" : ""}`}
                onClick={() => {
                  setFile(null);
                  go(["apps", data.summary.id, "guarantees", n.id, n.id]);
                }}
                aria-pressed={root.id === n.id}
              >
                <span className="app-guarantee-index">
                  {String(i + 1).padStart(2, "0")}
                </span>
                <span>
                  <b>{n.title}</b>
                  <span>{n.description}</span>
                </span>
                <span className="app-card-arrow">↗</span>
              </button>
            ))}
        </div>
        {guarantees.length > 40 && (
          <p className="app-muted">
            Showing up to 40 matches. Search by theorem name to narrow the list.
          </p>
        )}
      </div>
      <div className="app-explorer">
        <div className="app-map">
          <div className="app-map-toolbar">
            <div className="app-segment" aria-label="Graph lens">
              <button
                className={view === "proof" ? "is-on" : ""}
                onClick={() => setView("proof")}
              >
                Guarantee map
              </button>
              <button
                className={view === "flow" ? "is-on" : ""}
                onClick={() => setView("flow")}
              >
                Application flow
              </button>
            </div>
            <span className="app-muted">
              {model.author ? `Explanations: ${model.author}` : "Lean catalog"}
            </span>
          </div>
          <div className="app-map-caption">
            {latest?.run.declarations.length ? (
              <>
                <span className="app-lean-legend">
                  Solid cyan: recorded Lean dependencies
                </span>{" "}
                · {date(latest.run.started)} · {label(latest.freshness)} · Check{" "}
                {label(latest.run.status).toLowerCase()}
              </>
            ) : (
              <>
                Lean dependencies have not been recorded by the latest check.
                Run <code>h5i app check</code> to populate them.
              </>
            )}
            <br />
            Dashed gray: authored explanations and implementation links.
          </div>
          {latest &&
            (latest.freshness !== "matches_repository_inputs" ||
              latest.run.status !== "passed") && (
              <Note tone="warn">
                These dependencies describe the recorded run, not a verified
                current build. {label(latest.freshness)} ·{" "}
                {label(latest.run.status)}.
              </Note>
            )}
          <Graph
            model={model}
            root={root.id}
            selected={selected?.id ?? root.id}
            view={view}
            onSelect={select}
          />
          <div className="app-map-caption">
            Select a node to inspect its conditions, source and evidence. Lean
            edges are direct references in recorded declaration types and
            bodies. Constants outside the catalog are leaves.
          </div>
          {!!model.exclusions.length && (
            <details className="app-boundaries">
              <summary>
                Outside these guarantees · {model.exclusions.length}
              </summary>
              {model.exclusions.map((s) => (
                <p key={s}>{s}</p>
              ))}
            </details>
          )}
        </div>
        <aside className="app-inspector">
          {selected && (
            <>
              <span className={`app-kind kind-${selected.kind}`}>
                {selected.kind}
              </span>
              <h2>{selected.title}</h2>
              <p>{selected.description}</p>
              {selected.symbol && (
                <>
                  <code className="app-symbol">{selected.symbol}</code>
                  <button
                    className="app-button"
                    onClick={() => {
                      setFile(null);
                      go([
                        "apps",
                        data.summary.id,
                        "guarantees",
                        selected.id,
                        selected.id,
                      ]);
                    }}
                  >
                    Focus dependencies
                  </button>
                </>
              )}
              {selected.excludes.map((s) => (
                <Note key={s} tone="warn">
                  {s}
                </Note>
              ))}
              <h3>Connections</h3>
              <div className="app-relations">
                {related.map((e, i) => {
                  const other = model.nodes.find(
                    (n) => n.id === (e.from === selected.id ? e.to : e.from),
                  );
                  return (
                    <button key={i} onClick={() => other && select(other.id)}>
                      <span>
                        {e.from === selected.id ? "→" : "←"} {e.kind} ·{" "}
                        {e.origin === "lean" ? "Lean record" : "authored"}
                      </span>
                      <b>{other?.title}</b>
                    </button>
                  );
                })}
                {!related.length && (
                  <p className="app-muted">
                    No connections available in this explanation and catalog.
                  </p>
                )}
              </div>
              <h3>Source</h3>
              {selected.sources.map((s, i) => (
                <SourceLink
                  key={`${selected.id}:${i}`}
                  project={data.summary.id}
                  source={s}
                  onSelect={() => setFile(s)}
                />
              ))}
              {!selected.sources.length && (
                <p className="app-muted">
                  Follow a connection to the supporting source.
                </p>
              )}
              {file && (
                <SourceInspector project={data.summary.id} source={file} />
              )}
              {selected.symbol && (
                <>
                  <h3>Recorded Lean declaration</h3>
                  {declaration ? (
                    <>
                      <p className="app-muted">
                        {date(latest!.run.started)} · {label(latest!.freshness)}
                      </p>
                      <pre className="app-code">{declaration.signature}</pre>
                      {declaration.definition && (
                        <details>
                          <summary>Definition</summary>
                          <pre className="app-code">
                            {declaration.definition}
                          </pre>
                        </details>
                      )}
                      <p className="app-muted">
                        Axioms: {declaration.axioms.join(", ") || "none"}
                      </p>
                      <details>
                        <summary>
                          {declaration.dependencies.length} tool-observed
                          dependencies
                        </summary>
                        <pre className="app-code">
                          {declaration.dependencies.join("\n")}
                        </pre>
                      </details>
                    </>
                  ) : (
                    <p className="app-muted">
                      No declaration recorded for this symbol. An authored link
                      is not a proof result.
                    </p>
                  )}
                </>
              )}
              <Evidence view={latest} project={data.summary.id} />
            </>
          )}
        </aside>
      </div>
    </div>
  );
}

export function Graph({
  model,
  root,
  selected,
  view,
  onSelect,
}: {
  model: AppModel;
  root: string;
  selected: string;
  view: "proof" | "flow";
  onSelect: (id: string) => void;
}) {
  const [zoom, setZoom] = useState(1);
  useEffect(() => setZoom(1), [root, view]);
  const graph = useMemo(
    () => neighborhood(model, root, view),
    [model, root, view],
  );
  const columns =
    view === "proof"
      ? [
          ["guarantee"],
          ["theorem", "counterexample"],
          ["specification", "implementation"],
          ["assumption", "boundary"],
        ]
      : [
          ["boundary", "assumption"],
          ["implementation"],
          ["specification", "theorem", "counterexample", "guarantee"],
        ];
  const positions = new Map<string, { x: number; y: number }>();
  let rows = 1;
  columns.forEach((kinds, col) => {
    const nodes = graph.nodes.filter((n) => kinds.includes(n.kind));
    rows = Math.max(rows, nodes.length);
    nodes.forEach((n, row) =>
      positions.set(n.id, { x: 24 + col * 260, y: 40 + row * 126 }),
    );
  });
  const width = columns.length * 260 + 10,
    height = rows * 126 + 45;
  return (
    <>
      <div className="app-graph-actions">
        <span>
          {graph.nodes.length} of {graph.total} nodes · {graph.edges.length}{" "}
          connections
        </span>
        <div>
          <button
            aria-label="Zoom out"
            onClick={() => setZoom((z) => Math.max(0.6, z - 0.2))}
          >
            −
          </button>
          <button onClick={() => setZoom(1)}>Fit</button>
          <button
            aria-label="Zoom in"
            onClick={() => setZoom((z) => Math.min(2.4, z + 0.2))}
          >
            +
          </button>
        </div>
      </div>
      {graph.total > graph.nodes.length && (
        <Note>
          Showing the nearest 80 nodes. Select a connection in the inspector and
          use Focus dependencies to explore further.
        </Note>
      )}
      <div className="app-graph-scroll">
        {!graph.nodes.length ? (
          <Empty title="No application flow authored">
            <p>The guarantee map is still available.</p>
          </Empty>
        ) : (
          <svg
            className="app-graph"
            role="group"
            aria-label={
              view === "proof" ? "Guarantee relationships" : "Application flow"
            }
            viewBox={`0 0 ${width} ${height}`}
            style={{ width: `${zoom * 100}%`, minHeight: 240 }}
          >
            <defs>
              <marker
                id={`arrow-${view}`}
                viewBox="0 0 10 10"
                refX="9"
                refY="5"
                markerWidth="5"
                markerHeight="5"
                orient="auto-start-reverse"
              >
                <path d="M 0 0 L 10 5 L 0 10 z" fill="currentColor" />
              </marker>
            </defs>
            {graph.edges.map((e, i) => {
              const from = positions.get(e.from),
                to = positions.get(e.to);
              if (!from || !to) return null;
              const right = to.x > from.x;
              const x1 = from.x + (right ? 218 : 0),
                x2 = to.x + (right ? 0 : 218),
                y1 = from.y + 44,
                y2 = to.y + 44;
              const same = from.x === to.x;
              return (
                <g
                  key={i}
                  className={`app-graph-edge ${e.origin === "lean" ? "is-lean" : "is-authored"} ${e.from === selected || e.to === selected ? "is-active" : ""}`}
                >
                  <title>
                    {`${e.kind} · ${e.origin === "lean" ? "recorded Lean dependency" : "authored relationship"}`}
                  </title>
                  <path
                    d={
                      same
                        ? `M ${from.x + 218} ${y1} C ${from.x + 253} ${y1}, ${to.x + 253} ${y2}, ${to.x + 218} ${y2}`
                        : `M ${x1} ${y1} C ${(x1 + x2) / 2} ${y1}, ${(x1 + x2) / 2} ${y2}, ${x2} ${y2}`
                    }
                    markerEnd={`url(#arrow-${view})`}
                  />
                </g>
              );
            })}
            {graph.nodes.map((n) => {
              const pos = positions.get(n.id)!;
              const words = wrap(n.title, 25);
              return (
                <g
                  key={n.id}
                  role="button"
                  tabIndex={0}
                  aria-label={`${n.kind}: ${n.title}`}
                  aria-pressed={n.id === selected}
                  className={`app-graph-node kind-${n.kind} ${n.id === selected ? "is-on" : ""}`}
                  transform={`translate(${pos.x} ${pos.y})`}
                  onClick={() => onSelect(n.id)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" || e.key === " ") {
                      e.preventDefault();
                      onSelect(n.id);
                    }
                  }}
                >
                  <title>{n.title}</title>
                  <rect width="218" height="88" rx="5" />
                  <text className="app-node-kind" x="14" y="22">
                    {n.kind.toUpperCase()}
                  </text>
                  {words.slice(0, 2).map((line, i) => (
                    <text
                      key={i}
                      className="app-node-title"
                      x="14"
                      y={45 + i * 17}
                    >
                      {line}
                    </text>
                  ))}
                </g>
              );
            })}
          </svg>
        )}
      </div>
    </>
  );
}
function wrap(s: string, width: number) {
  const lines = [""];
  for (const word of s
    .split(" ")
    .flatMap((w) => w.match(new RegExp(`.{1,${width}}`, "gu")) ?? [])) {
    const last = lines.length - 1;
    if ((lines[last] + word).length > width && lines[last]) lines.push(word);
    else lines[last] += `${lines[last] ? " " : ""}${word}`;
  }
  return lines;
}

export function MapPrompt({ project }: { project: string }) {
  return (
    <section className="app-map-prompt">
      <h3>Ask your agent to explain this app</h3>
      <p className="app-muted">
        A guarantee map connects user-visible behavior to its code, proofs and
        conditions. Try this prompt in your coding agent:
      </p>
      <pre className="app-code">{`Create h5i-app.ui.json next to the manifest for ${project}.
Use docs/app/app-console.md and examples/app/booking/h5i-app.ui.json as references.
Read the Rust and Lean code. Explain the user-visible guarantees, their assumptions
and exclusions, and connect them to the implemented specifications, theorems and
Rust code. Include an application-flow view where useful.
Use exact Lean symbols and repository-relative source paths with unique anchors.
Lean dependency edges come from check records; do not invent proof evidence or
claim that unverified behavior is guaranteed.`}</pre>
    </section>
  );
}

function SourceLink({
  project,
  source,
  onSelect,
}: {
  project: string;
  source: AppSource;
  onSelect: () => void;
}) {
  const [location, setLocation] = useState<SourceView | null>(null);
  const [error, setError] = useState(false);
  useEffect(() => {
    const c = new AbortController();
    setLocation(null);
    setError(false);
    appsApi
      .source(project, source, c.signal)
      .then((value) => {
        if (!c.signal.aborted) setLocation(value);
      })
      .catch(() => {
        if (!c.signal.aborted) setError(true);
      });
    return () => c.abort();
  }, [project, source.path, source.anchor]);
  return (
    <button className="app-source-link" onClick={onSelect} title={source.path}>
      <code>
        {source.path.split("/").slice(-2).join("/")}
        {location?.anchor_line != null ? `:${location.anchor_line}` : ""}
      </code>
      <span>{source.anchor || "Open file"} ↗</span>
      {source.anchor && (
        <span>
          {error
            ? "Location unavailable"
            : !location
              ? "Locating line…"
              : location.anchor_line == null
                ? "Anchor missing or ambiguous"
                : `Line ${location.anchor_line} · working tree`}
        </span>
      )}
    </button>
  );
}
function SourceInspector({
  project,
  source,
}: {
  project: string;
  source: AppSource;
}) {
  const [data, setData] = useState<SourceView | null>(null),
    [error, setError] = useState("");
  useEffect(() => {
    const c = new AbortController();
    setData(null);
    setError("");
    appsApi
      .source(project, source, c.signal)
      .then(setData)
      .catch((e) => {
        if (!c.signal.aborted) setError(String(e));
      });
    return () => c.abort();
  }, [project, source.path, source.anchor]);
  return (
    <div className="app-source">
      {error ? (
        <Note tone="warn">{error}</Note>
      ) : !data ? (
        <Spin label="Reading source" />
      ) : (
        <>
          <div className="app-source-heading">
            <code>{data.path}</code>
            <span>
              {data.revision} · lines {data.start_line}–
              {data.start_line + data.text.split("\n").length - 1} /{" "}
              {data.total_lines}
            </span>
          </div>
          {!data.anchor_found && (
            <Note tone="warn">
              The source anchor is missing or ambiguous. Review this
              explanation.
            </Note>
          )}
          <pre className="app-code app-numbered">
            {data.text.split("\n").map((s, i) => (
              <span key={i}>
                <i>{data.start_line + i}</i>
                {s}
                {"\n"}
              </span>
            ))}
          </pre>
        </>
      )}
    </div>
  );
}

function Evidence({ view, project }: { view?: RunView; project: string }) {
  return (
    <section className="app-evidence">
      <h3>Check evidence</h3>
      {view ? (
        <>
          <p>
            {label(view.run.status)} · {date(view.run.started)}
          </p>
          <p className="app-muted">
            {label(view.freshness)} · {short(view.run.inputs.head)}
          </p>
          {view.run.error && <Note tone="warn">{view.run.error}</Note>}
          <Stages stages={view.run.stages} />
          {!!view.run.unbuilt_modules.length && (
            <Note tone="warn">
              Not gated: {view.run.unbuilt_modules.join(", ")}
            </Note>
          )}
        </>
      ) : (
        <p className="app-muted">No check has been recorded.</p>
      )}
      <Cmd text={`h5i app check ${shellQuote(project)}`} />
      <p className="app-muted">
        Checks built Lean modules and their axioms. Extraction freshness and
        deployment conditions require separate evidence.
      </p>
    </section>
  );
}

function Stages({ stages }: { stages: AppStage[] }) {
  return (
    <div className="app-stages">
      {stages.map((s, i) => (
        <details key={i}>
          <summary>
            <span
              className={`app-stage-dot ${warn(s.status) ? "is-warn" : s.status === "passed" ? "is-pass" : ""}`}
            />
            <b>{label(s.name)}</b>
            <span>{label(s.status)}</span>
            <time>{s.seconds.toFixed(1)}s</time>
          </summary>
          <pre className="app-code">
            {s.log || "No diagnostic output recorded."}
          </pre>
        </details>
      ))}
    </div>
  );
}

function Changes({ project, refresh }: { project: string; refresh: number }) {
  const [baseline, setBaseline] = useState("HEAD"),
    [input, setInput] = useState("HEAD"),
    [epoch, setEpoch] = useState(0);
  const [data, setData] = useState<AppHistory | null>(null),
    [error, setError] = useState("");
  useEffect(() => {
    const c = new AbortController();
    setData(null);
    setError("");
    appsApi
      .history(project, baseline, c.signal)
      .then(setData)
      .catch((e) => {
        if (!c.signal.aborted) setError(String(e));
      });
    return () => c.abort();
  }, [project, baseline, epoch, refresh]);
  return (
    <div className="app-scroll app-pad">
      <div className="app-view-heading">
        <div>
          <span className="app-eyebrow">Review the delta</span>
          <h2>What changed under the guarantee?</h2>
          <p className="app-muted">
            Conditions, definitions and authored relationships, with affected
            guarantees.
          </p>
        </div>
        <form
          onSubmit={(e) => {
            e.preventDefault();
            setBaseline(input.trim() || "HEAD");
            setEpoch((n) => n + 1);
          }}
        >
          <label htmlFor="app-baseline">Compare working tree with</label>
          <div>
            <input
              id="app-baseline"
              value={input}
              onChange={(e) => setInput(e.target.value)}
              placeholder="HEAD, main, or commit"
            />
            <button className="app-button" type="submit">
              Compare
            </button>
          </div>
        </form>
      </div>
      {error && <Note tone="warn">{error}</Note>}
      {!data && !error && <Spin label="Reading Git history" />}
      {data && (
        <>
          <div className="app-comparison-head">
            <h3>Working tree ← {short(data.baseline)}</h3>
            <Chip tone={data.comparison.length ? "warn" : "dim"}>
              {data.comparison.length} review items
            </Chip>
          </div>
          {data.comparison.length ? (
            <ChangeList changes={data.comparison} />
          ) : (
            <p className="app-empty-inline">
              No changes in the watched sources and explanation. Semantic
              equivalence has not been established.
            </p>
          )}
          <div className="app-history-heading">
            <h3>Elaborated declarations</h3>
          </div>
          <p className="app-muted">{data.catalog_note}</p>
          <ChangeList changes={data.catalog_comparison} />
          <div className="app-history-heading">
            <h3>Earlier changes</h3>
            <span className="app-muted">
              {data.scanned} commits scanned · first parent
            </span>
          </div>
          {(data.truncated || data.shallow) && (
            <Note tone="warn">
              {data.truncated ? "The scan reached its 120-commit limit. " : ""}
              {data.shallow
                ? "This is a shallow clone; earlier history is unavailable."
                : ""}
            </Note>
          )}
          {data.revisions.map((r) => (
            <details key={r.commit} className="app-revision">
              <summary>
                <code>{short(r.commit)}</code>
                <b>{r.message}</b>
                <span>{r.changes.length} changes</span>
                <time>{new Date(r.time * 1000).toLocaleDateString()}</time>
              </summary>
              <ChangeList changes={r.changes} />
            </details>
          ))}
          {!data.revisions.length && (
            <p className="app-empty-inline">
              No changes found in the scanned history.
            </p>
          )}
          <div className="app-reading-notes">
            {data.notes.map((n) => (
              <p key={n}>{n}</p>
            ))}
          </div>
        </>
      )}
    </div>
  );
}
function ChangeList({ changes }: { changes: AppChange[] }) {
  return (
    <div className="app-changes">
      {changes.map((c, i) => (
        <details key={i} className="app-change">
          <summary>
            <span className="app-change-marker">Δ</span>
            <span>
              <b>{c.title}</b>
              <small>{c.reason}</small>
            </span>
            {!!c.affected.length && (
              <span className="app-affected">{c.affected.length} affected</span>
            )}
          </summary>
          <div className="app-change-body">
            {c.path && <code>{c.path}</code>}
            {!!c.affected.length && <p>Affects: {c.affected.join(" · ")}</p>}
            <div className="app-diff">
              <div>
                <span>Before</span>
                <pre>{c.before || "[not present]"}</pre>
              </div>
              <div>
                <span>After</span>
                <pre>{c.after || "[not present]"}</pre>
              </div>
            </div>
          </div>
        </details>
      ))}
    </div>
  );
}

function Mutations({ data }: { data: AppDetail }) {
  const runs = data.runs.filter((v) => v.run.kind === "mutation");
  const [runId, setRunId] = useState(""),
    [trialName, setTrialName] = useState(""),
    [filter, setFilter] = useState("all");
  const view = runs.find((v) => v.run.id === runId) ?? runs[0];
  const run = view?.run;
  const trial =
    run?.trials.find((t) => t.name === trialName) ??
    run?.trials.find((t) => t.status === "survived") ??
    run?.trials[0];
  if (!run)
    return (
      <div className="app-pad">
        <Empty title="No mutation run recorded">
          <p>
            Challenge the specification by deliberately changing the Rust,
            extracting it again, and rebuilding the proofs. Each run will appear
            here with its edits, stages and diagnostics.
          </p>
          <Cmd text={`h5i app mutate ${shellQuote(data.summary.id)}`} />
        </Empty>
        <p className="app-muted">
          {data.runs_total > data.runs.length
            ? `Showing the newest ${data.runs.length} of ${data.runs_total} check and mutation records.`
            : "Existing terminal output cannot reconstruct a run's evidence."}
        </p>
      </div>
    );
  const counts = new Map<string, number>();
  for (const t of run.trials.filter((t) => t.name !== "baseline"))
    counts.set(t.status, (counts.get(t.status) ?? 0) + 1);
  return (
    <div className="app-scroll app-pad">
      <div className="app-view-heading">
        <div>
          <span className="app-eyebrow">Challenge the specification</span>
          <h2>Mutation experiments</h2>
          <p className="app-muted">
            A surviving change is a review candidate. A failed build is not
            proof coverage.
          </p>
        </div>
        <select
          aria-label="Mutation run"
          value={run.id}
          onChange={(e) => {
            setRunId(e.target.value);
            setTrialName("");
          }}
        >
          {runs.map((v) => (
            <option key={v.run.id} value={v.run.id}>
              {date(v.run.started)} · {label(v.run.status)}
            </option>
          ))}
        </select>
      </div>
      <div className="app-run-banner">
        <Chip tone={warn(run.status) ? "warn" : "dim"}>
          {label(run.status)}
        </Chip>
        <span>{label(view.freshness)}</span>
        <code>{short(run.inputs.head)}</code>
        <span>Baseline: {label(run.baseline)}</span>
      </div>
      {run.baseline !== "passed" && (
        <Note tone="warn">
          A passing baseline is required to interpret mutation outcomes. This
          run's baseline is {label(run.baseline).toLowerCase()}.
        </Note>
      )}
      {run.error && <Note tone="warn">{run.error}</Note>}
      <div className="app-mutation-counts">
        {[...counts].map(([s, n]) => (
          <button
            key={s}
            aria-pressed={filter === s}
            className={filter === s ? "is-on" : ""}
            onClick={() => setFilter(filter === s ? "all" : s)}
          >
            <b>{n}</b>
            <span>{label(s)}</span>
          </button>
        ))}
      </div>
      <div className="app-trial-grid">
        <div className="app-trial-list">
          <div className="app-section-heading">
            <h3>Selected experiments</h3>
            <button
              className="app-text-button"
              onClick={() => setFilter("all")}
            >
              Show all
            </button>
          </div>
          {run.trials
            .filter((t) => filter === "all" || t.status === filter)
            .map((t) => (
              <button
                className={`app-trial ${trial === t ? "is-on" : ""}`}
                key={t.name}
                onClick={() => setTrialName(t.name)}
              >
                <b>{t.name}</b>
                <span className={warn(t.status) ? "app-warning" : "app-muted"}>
                  {label(t.status)} ·{" "}
                  {t.name === "baseline"
                    ? "unmodified"
                    : t.generated
                      ? "generated"
                      : "declared"}
                </span>
              </button>
            ))}
        </div>
        {trial && (
          <article className="app-trial-detail">
            <h3>{trial.name}</h3>
            <p className="app-muted">
              {trial.file || "Unmodified project in a separate copy"}
            </p>
            {trial.status === "proof_failed" && (
              <Note>
                Lean build failed after Rust checking and extraction. Read the
                diagnostics before attributing this failure to a theorem.
              </Note>
            )}
            {trial.status === "survived" && (
              <Note tone="warn">
                The proofs still build. Review whether this behavior is
                required, intentionally outside the specification, or
                equivalent.
              </Note>
            )}
            {trial.edits.map((edit, i) => (
              <div className="app-diff" key={i}>
                <div>
                  <span>Original</span>
                  <pre>{edit.before}</pre>
                </div>
                <div>
                  <span>Mutation</span>
                  <pre>{edit.after || "[removed]"}</pre>
                </div>
              </div>
            ))}
            <Stages stages={trial.stages} />
            {!trial.stages.length && (
              <p className="app-empty-inline">No stages executed.</p>
            )}
          </article>
        )}
      </div>
      <details className="app-record-meta">
        <summary>Run identity, selection and toolchain</summary>
        <p>
          <code>{run.id}</code>
        </p>
        <p>Targets: {run.targets.join(", ") || "Lake default targets"}</p>
        <p>
          Input digest: <code>{run.inputs.digest}</code>
        </p>
        {Object.entries(run.toolchain).map(([k, v]) => (
          <p key={k}>
            {k}: <code>{v}</code>
          </p>
        ))}
        <p>
          Finished:{" "}
          {run.finished
            ? date(run.finished)
            : "No completion recorded; the process may still be running or may have been interrupted."}
        </p>
        <p>
          Repository-wide input matching is conservative. External dependencies
          and deployed conditions are not certified by this fingerprint.
        </p>
      </details>
      <p className="app-muted">
        Showing up to 20 recent check and mutation records ({data.runs_total} on
        disk).
      </p>
      <Cmd text={`h5i app mutate ${shellQuote(data.summary.id)}`} />
    </div>
  );
}
