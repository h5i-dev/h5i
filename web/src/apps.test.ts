import { describe, expect, it } from "vitest";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { Graph } from "./Apps";
import {
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
});
