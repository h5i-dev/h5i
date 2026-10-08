import { describe, expect, it } from "vitest";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { Graph, MapPrompt } from "./Apps";
import {
  explanationDrift,
  flowLayers,
  focusGap,
  neighborhood,
  shellQuote,
  withLeanDependencies,
  type AppModel,
  type Declaration,
} from "./apps-api";
const model: AppModel = {
  version: 1,
  title: "",
  description: "",
  author: "agent",
  exclusions: [],
  nodes: ["g", "t", "a", "other"].map((id) => ({
    id,
    kind: "theorem",
    title: id,
    description: "",
    sources: [],
    excludes: [],
  })),
  edges: [
    { from: "g", to: "t", kind: "proved by", view: "proof" },
    { from: "t", to: "a", kind: "assumes", view: "proof" },
    { from: "a", to: "t", kind: "references", view: "proof" },
    { from: "other", to: "a", kind: "input", view: "flow" },
  ],
};
describe("app review navigation", () => {
  it("offers an agent prompt instead of a source inventory when no map exists", () => {
    const html = renderToStaticMarkup(
      createElement(MapPrompt, { project: "examples/app/inbox" }),
    );
    expect(html).toContain("examples/app/inbox");
    expect(html).toContain("h5i-app.ui.json");
    expect(html).toContain("unique anchors");
    expect(html).toContain("do not invent proof evidence");
    expect(html).not.toContain("Required theorem names");
    expect(html).not.toContain("Source inventory");
  });
  it("shows indirect dependencies without pulling unrelated flows into a guarantee", () => {
    expect(neighborhood(model, "g", "proof").nodes.map((n) => n.id)).toEqual([
      "g",
      "t",
      "a",
    ]);
    expect(neighborhood(model, "g", "flow").nodes.map((n) => n.id)).toEqual([
      "a",
      "other",
    ]);
  });
  it("quotes project paths as one shell argument", () => {
    expect(shellQuote("a'b $(touch x)")).toBe("'a'\"'\"'b $(touch x)'");
  });
});

const declaration = (
  name: string,
  dependencies: string[] = [],
  kind = "theorem",
): Declaration => ({
  name,
  dependencies,
  kind,
  signature: "Prop",
  definition: null,
  axioms: [],
});
function authored(): AppModel {
  return {
    ...model,
    nodes: model.nodes.map((n) => ({
      ...n,
      kind:
        n.id === "g"
          ? "guarantee"
          : n.id === "other"
            ? "implementation"
            : "theorem",
      symbol:
        n.id === "t" ? "App.safe" : n.id === "a" ? "App.oldCondition" : null,
    })),
  };
}

describe("Lean-derived dependency graph", () => {
  it("renders tool provenance separately from authored relationships", () => {
    const graph = withLeanDependencies(authored(), [
      declaration("App.safe", ["True"]),
    ]);
    const html = renderToStaticMarkup(
      createElement(Graph, {
        model: graph,
        root: "g",
        selected: "t",
        view: "proof",
        onSelect: () => {},
      }),
    );
    expect(html).toContain("app-graph-edge is-lean");
    expect(html).toContain("app-graph-edge is-authored");
    expect(html).toContain("recorded Lean dependency");
    expect(html).toContain("specification: True");
  });
  it("replaces authored Lean edges with direct catalog references and adds unnamed helpers", () => {
    const graph = withLeanDependencies(authored(), [
      declaration("App.safe", ["App.helper", "App.helper", "App.safe"]),
      declaration("App.helper", ["Nat.add"], "definition"),
    ]);
    const helper = graph.nodes.find((n) => n.symbol === "App.helper")!;
    expect(helper.kind).toBe("specification");
    expect(
      graph.nodes.find((n) => n.symbol === "Nat.add")?.description,
    ).toContain("outside this catalog");
    expect(graph.edges.filter((e) => e.origin === "lean")).toHaveLength(2);
    expect(graph.edges.some((e) => e.from === "t" && e.to === "a")).toBe(false);
    expect(
      graph.edges.some(
        (e) => e.from === "g" && e.to === "t" && e.origin === "authored",
      ),
    ).toBe(true);
    expect(
      graph.edges.some((e) => e.view === "flow" && e.origin === "authored"),
    ).toBe(true);
    expect(
      neighborhood(graph, "g", "proof").nodes.map((n) => n.symbol),
    ).toContain("Nat.add");
  });
  it("does not turn manually declared dependencies into evidence when a catalog is missing", () => {
    const graph = withLeanDependencies(authored(), []);
    expect(graph.edges.filter((e) => e.origin === "lean")).toHaveLength(0);
    expect(graph.edges.some((e) => e.from === "t" && e.to === "a")).toBe(false);
  });
  it("says why Focus dependencies has nothing to follow", () => {
    const missing = withLeanDependencies(authored(), []);
    expect(focusGap(missing, "g", [], "p")).toBeNull();
    expect(focusGap(missing, "t", [], "p")).toContain("h5i app check p");
    const decls = [declaration("App.safe", ["Nat.add"])];
    const graph = withLeanDependencies(authored(), decls);
    expect(focusGap(graph, "t", decls, "p")).toBeNull();
    const leaf = graph.nodes.find((n) => n.symbol === "Nat.add")!.id;
    expect(focusGap(graph, leaf, decls, "p")).toContain(
      "outside the recorded catalog",
    );
  });
  it("works without an authored explanation and terminates on cycles", () => {
    const graph = withLeanDependencies(null, [
      declaration("App.a", ["App.b"]),
      declaration("App.b", ["App.a"]),
    ]);
    const root = graph.nodes.find((n) => n.symbol === "App.a")!.id;
    expect(neighborhood(graph, root, "proof").nodes).toHaveLength(2);
  });
  it("retains source anchors and supports multiple explanatory nodes for one symbol", () => {
    const m = authored();
    m.nodes.find((n) => n.id === "t")!.sources = [
      { path: "Proofs.lean", anchor: "theorem safe" },
    ];
    m.nodes.push({ ...m.nodes.find((n) => n.id === "t")!, id: "alias" });
    const graph = withLeanDependencies(m, [declaration("App.safe", ["True"])]);
    expect(graph.nodes.find((n) => n.id === "t")?.sources[0].path).toBe(
      "Proofs.lean",
    );
    expect(
      graph.edges.filter((e) => e.origin === "lean").map((e) => e.from),
    ).toEqual(["t", "alias"]);
  });
  it("bounds rendered neighborhoods while allowing exploration from a deeper root", () => {
    const graph = withLeanDependencies(
      null,
      Array.from({ length: 100 }, (_, i) =>
        declaration(`App.t${i}`, i < 99 ? [`App.t${i + 1}`] : []),
      ),
    );
    const root = graph.nodes[0].id;
    expect(neighborhood(graph, root, "proof").total).toBe(100);
    expect(neighborhood(graph, root, "proof").nodes).toHaveLength(80);
    expect(neighborhood(graph, graph.nodes[79].id, "proof").nodes).toHaveLength(
      21,
    );
  });
  it("reads explanation drift from source issues", () => {
    const src = (digest?: string) => ({ path: "a.rs", anchor: "fn a", digest });
    const m: AppModel = {
      ...model,
      nodes: [
        { ...model.nodes[0], id: "x", title: "Kept", sources: [src("d")] },
        { ...model.nodes[0], id: "y", title: "Moved", sources: [src("d")] },
        { ...model.nodes[0], id: "z", title: "Loose", sources: [src()] },
        { ...model.nodes[0], id: "w", title: "Bare", sources: [] },
      ],
    };
    const d = explanationDrift(m, [
      "Moved: explanation's source fingerprint is stale",
      "Moved: source anchor missing or ambiguous in a.rs",
      "Unrelated: explanation's source fingerprint is stale",
      "check record unreadable",
    ]);
    expect(d.cited).toBe(3);
    expect(d.fingerprinted).toBe(2);
    expect([...d.drifted.keys()]).toEqual(["Moved"]);
    expect(d.drifted.get("Moved")).toHaveLength(2);
    expect(explanationDrift(null, ["x"]).cited).toBe(0);
  });
  it("lays the flow out by step, ignoring the edge that closes a reply", () => {
    const n = (id: string) => ({ ...model.nodes[0], id, title: id });
    const e = (from: string, to: string) => ({
      from,
      to,
      kind: "calls",
      view: "flow" as const,
    });
    expect(
      flowLayers(["store", "kernel", "route", "auth", "handler"].map(n), [
        e("route", "auth"),
        e("auth", "handler"),
        e("route", "handler"),
        e("handler", "kernel"),
        e("kernel", "store"),
        e("store", "route"),
      ]),
    ).toEqual([["route"], ["auth"], ["handler"], ["kernel"], ["store"]]);
  });
});
