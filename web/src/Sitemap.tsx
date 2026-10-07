import { useMemo, useState } from "react";

import type { SessionDetail, SiteEndpoint, SiteOrigin } from "./api";
import { Chip, Cmd, Empty, Facts, Method, Note, ReqLink, Split, plural, type SessionNav } from "./ui";

// What this session reached, as a tree: origin, then path segments, with the
// counts folded upward. Refusals sit apart from hits at every level, because
// "recon wanted to reach this and was not allowed" is a fact a reviewer wants.
// Paths that only ever answered 404 are left off: a sweep's misses bury the
// few paths that exist. `h5i websec sitemap` still carries them.

interface Node {
  key: string;
  label: string;
  /** Full path for a leaf; a prefix for a directory. */
  path: string;
  origin: string;
  hits: number;
  refused: number;
  navigated: boolean;
  leaf?: SiteEndpoint;
  children: Map<string, Node>;
}

function onlyMissed(e: SiteEndpoint): boolean {
  return e.refused === 0 && e.statuses.length > 0 && e.statuses.every((s) => s === 404);
}

/** The sitemap with 404-only paths dropped, and origins left empty by that. */
export function visibleSitemap(detail: SessionDetail): { origins: SiteOrigin[]; hidden: number } {
  let hidden = 0;
  const origins: SiteOrigin[] = [];
  for (const o of detail.sitemap) {
    const endpoints = o.endpoints.filter((e) => !onlyMissed(e));
    const dropped = o.endpoints.length - endpoints.length;
    hidden += dropped;
    if (dropped > 0 && endpoints.length === 0) continue;
    const lost = o.endpoints.filter(onlyMissed).reduce((n, e) => n + e.hits, 0);
    origins.push({ ...o, hits: o.hits - lost, endpoints });
  }
  return { origins, hidden };
}

function build(sitemap: SiteOrigin[]): Node[] {
  return sitemap.map((o) => {
    const root: Node = {
      key: o.origin,
      label: o.origin,
      path: "",
      origin: o.origin,
      hits: o.hits,
      refused: o.refused,
      navigated: false,
      children: new Map(),
    };
    for (const e of o.endpoints) {
      const segs = e.path.split("/").filter(Boolean);
      let at = root;
      let prefix = "";
      for (let i = 0; i < segs.length; i += 1) {
        prefix += `/${segs[i]}`;
        const last = i === segs.length - 1;
        const label = segs[i] + (last && e.path.endsWith("/") ? "/" : "");
        let next = at.children.get(label);
        if (!next) {
          next = {
            key: `${o.origin}${prefix}${last ? "" : "/"}`,
            label,
            path: last ? e.path : `${prefix}/`,
            origin: o.origin,
            hits: 0,
            refused: 0,
            navigated: false,
            children: new Map(),
          };
          at.children.set(label, next);
        }
        next.hits += e.hits;
        next.refused += e.refused;
        next.navigated = next.navigated || e.navigated;
        if (last) next.leaf = e;
        at = next;
      }
      if (segs.length === 0) {
        // The origin's own root path.
        const label = "/";
        let next = root.children.get(label);
        if (!next) {
          next = { key: `${o.origin}/`, label, path: "/", origin: o.origin, hits: 0, refused: 0, navigated: false, children: new Map() };
          root.children.set(label, next);
        }
        next.hits += e.hits;
        next.refused += e.refused;
        next.navigated = next.navigated || e.navigated;
        next.leaf = e;
      }
    }
    return root;
  });
}

export function Sitemap({ detail, nav }: { detail: SessionDetail; nav: SessionNav }) {
  const { origins, hidden } = useMemo(() => visibleSitemap(detail), [detail]);
  const roots = useMemo(() => build(origins), [origins]);
  const [open, setOpen] = useState<Set<string>>(() => new Set(roots.slice(0, 3).map((r) => r.key)));
  const [selected, setSelected] = useState<Node | null>(null);

  if (roots.length === 0) {
    return (
      <Empty title={hidden > 0 ? "Nothing but 404s" : "Nothing reached yet"}>
        <p>The map is folded from the request log, so it fills in as the session fetches.</p>
        {hidden > 0 ? <p>
            {hidden} {plural(hidden, "path")} that answered only 404 {hidden === 1 ? "is" : "are"} left off.
          </p> : null}
      </Empty>
    );
  }

  const toggle = (n: Node) => {
    const next = new Set(open);
    if (next.has(n.key)) next.delete(n.key);
    else next.add(n.key);
    setOpen(next);
  };

  const tree = (
    <div className="scroll">
      <div className="tree" role="tree">
        {roots.map((r) => (
          <TreeNode key={r.key} node={r} depth={0} open={open} onToggle={toggle} selected={selected} onSelect={setSelected} />
        ))}
      </div>
    </div>
  );

  return (
    <Split
      id="sitemap"
      initial={460}
      first={tree}
      second={
        selected ? (
          <NodeDetail node={selected} detail={detail} nav={nav} />
        ) : (
          <Empty title="Pick a path">
            <p>
              Counts fold upward: a directory shows every fetch beneath it. A red count is fetches policy refused
              before the wire.
            </p>
            {hidden > 0 ? (
              <p>
                {hidden} {plural(hidden, "path")} that answered only 404 {hidden === 1 ? "is" : "are"} left off.
              </p>
            ) : null}
            <Cmd text={`h5i websec sitemap --session ${detail.name ?? detail.id}`} hint="the same fold, as JSON" />
          </Empty>
        )
      }
    />
  );
}

function TreeNode({
  node,
  depth,
  open,
  onToggle,
  selected,
  onSelect,
}: {
  node: Node;
  depth: number;
  open: Set<string>;
  onToggle: (n: Node) => void;
  selected: Node | null;
  onSelect: (n: Node) => void;
}) {
  const kids = [...node.children.values()].sort((a, b) => b.hits + b.refused - (a.hits + a.refused) || a.label.localeCompare(b.label));
  const branch = kids.length > 0;
  const isOpen = open.has(node.key);
  const cls = `tree-node${depth === 0 ? " tree-origin" : ""}${node.leaf && !branch ? " tree-leaf" : ""}${selected?.key === node.key ? " is-on" : ""}`;
  return (
    <div role="treeitem" aria-expanded={branch ? isOpen : undefined}>
      <button
        type="button"
        className={cls}
        onClick={() => {
          onSelect(node);
          if (branch && !isOpen) onToggle(node);
        }}
        onDoubleClick={() => branch && onToggle(node)}
      >
        <span
          className="tree-toggle"
          onClick={(e) => {
            if (!branch) return;
            e.stopPropagation();
            onToggle(node);
          }}
          aria-hidden
        >
          {branch ? (isOpen ? "▾" : "▸") : ""}
        </span>
        <span className="tree-label" title={node.path || node.origin}>
          {node.label}
        </span>
        <span className="tree-counts">
          {node.navigated ? (
            <span className="nav" title="reached by a navigation">
              nav
            </span>
          ) : null}
          {node.leaf && node.leaf.methods.length > 1 ? <span>{node.leaf.methods.join(" ")}</span> : null}
          {node.hits > 0 ? <span title="allowed fetches">{node.hits}</span> : null}
          {node.refused > 0 ? (
            <span className="refused" title="refused before the wire">
              {node.refused}
            </span>
          ) : null}
        </span>
      </button>
      {branch && isOpen ? (
        <div className="tree-children" role="group">
          {kids.map((k) => (
            <TreeNode key={k.key} node={k} depth={depth + 1} open={open} onToggle={onToggle} selected={selected} onSelect={onSelect} />
          ))}
        </div>
      ) : null}
    </div>
  );
}

function NodeDetail({ node, detail, nav }: { node: Node; detail: SessionDetail; nav: SessionNav }) {
  const name = detail.name ?? detail.id;
  const leaf = node.leaf;
  const under = [...walk(node)].filter((n) => n.leaf).map((n) => n.leaf as SiteEndpoint);
  const host = node.origin.replace(/^https?:\/\//, "");
  return (
    <div className="scroll">
      <div className="node-detail">
        <h3>
          {node.origin}
          {node.path}
        </h3>
        <div className="chips">
          <Chip tone="plain">
            <b>{node.hits}</b> allowed
          </Chip>
          {node.refused > 0 ? (
            <Chip tone="bad">
              <b>{node.refused}</b> refused
            </Chip>
          ) : null}
          {node.navigated ? <Chip tone="violet">navigated to</Chip> : null}
          {!leaf && under.length > 0 ? (
            <Chip tone="dim">
              <b>{under.length}</b> {plural(under.length, "path")} beneath
            </Chip>
          ) : null}
        </div>

        {leaf ? (
          <Facts
            rows={[
              ["methods", <span key="m" style={{ display: "flex", gap: 8 }}>{leaf.methods.map((m) => <Method key={m} m={m} />)}</span>],
              ["statuses", leaf.statuses.length ? leaf.statuses.join(", ") : "none answered"],
              ["parameters", leaf.params.length ? <code key="p">{leaf.params.join(", ")}</code> : "none seen"],
              ["last fetch", <ReqLink key="l" id={`req_${leaf.last_seq}`} nav={nav} />],
            ]}
          />
        ) : null}

        {!leaf && under.length > 0 ? (
          <div className="table-wrap" style={{ flex: "none", maxHeight: 420, border: "1px solid var(--line)", borderRadius: 4 }}>
            <table className="grid">
              <thead>
                <tr>
                  <th>path</th>
                  <th>methods</th>
                  <th>statuses</th>
                  <th className="num">hits</th>
                  <th className="num">refused</th>
                </tr>
              </thead>
              <tbody>
                {under.slice(0, 300).map((e) => (
                  <tr key={e.path}>
                    <td className="cell-path" title={e.path}>
                      {e.path}
                      {e.params.length ? <span className="path-q">?{e.params.join("&")}</span> : null}
                    </td>
                    <td>{e.methods.join(" ")}</td>
                    <td className="dim">{e.statuses.join(" ")}</td>
                    <td className="num">{e.hits}</td>
                    <td className="num">{e.refused > 0 ? <span className="status status-refused">{e.refused}</span> : ""}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        ) : null}

        {leaf && leaf.refused > 0 && leaf.hits === 0 ? (
          <Note tone="bad">
            Every fetch of this path was refused before the wire. The History tab's inspector carries the reason
            on each one.
          </Note>
        ) : null}

        <div style={{ display: "flex", flexDirection: "column", gap: 6, alignItems: "flex-start" }}>
          <Cmd text={`h5i websec requests --session ${name} --host ${host}`} hint="every fetch of this origin" />
          {leaf ? <Cmd text={`h5i websec show req_${leaf.last_seq} --session ${name}`} hint="the newest message on this path" /> : null}
          {detail.ledger ? (
            <Cmd text={`h5i recon endpoints --session ${name} --origin ${node.origin}`} hint="what the ledger says exists here" />
          ) : null}
        </div>
      </div>
    </div>
  );
}

function* walk(n: Node): Generator<Node> {
  yield n;
  for (const k of n.children.values()) yield* walk(k);
}
