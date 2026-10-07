import { describe, expect, it } from "vitest";
import { neighborhood, shellQuote, type AppModel } from "./apps-api";
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
