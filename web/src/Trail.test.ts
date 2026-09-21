import { describe, expect, it } from "vitest";

import type { SessionRow } from "./api";
import { captionOf, nodesOf } from "./Trail";

function live(over: Partial<SessionRow>): SessionRow {
  return {
    id: "br_1",
    name: "scan",
    state: "live",
    placement: "host",
    lane: "direct",
    identity: "native",
    url: "https://shop.example.com/cart",
    started_at: "2026-09-21T08:00:00Z",
    ended_at: null,
    end_reason: null,
    engine: "h5i",
    confinement: "process",
    enclosing_box: null,
    permissive_cors: false,
    project: null,
    policy_digest: "d",
    scope_digest: "",
    restored_from: null,
    expires_at: null,
    held_by_human: false,
    requests: 0,
    denied: 0,
    origins: [],
    last_request_at: null,
    captured: null,
    reclaimed: null,
    ledger: null,
    jobs: [],
    findings: 0,
    verbs: 0,
    attention: { state: "working", why: "" },
    ...over,
  };
}

describe("nodesOf", () => {
  it("draws one node per host and sums that host's findings", () => {
    const nodes = nodesOf(
      [
        live({ url: "https://a.example.com/x", findings: 1 }),
        live({ url: "https://a.example.com/y", findings: 2 }),
        live({ url: "https://b.example.com/", findings: 0 }),
      ],
      false,
    );
    expect(nodes.map((n) => [n.host, n.findings])).toEqual([
      ["a.example.com", 3],
      ["b.example.com", 0],
    ]);
    expect(nodes[0].x).toBeLessThan(nodes[1].x);
  });

  it("folds the tail into one node rather than crowding the band", () => {
    const many = Array.from({ length: 12 }, (_, i) =>
      live({ url: `https://h${i}.example.com/`, findings: 1 }),
    );
    const nodes = nodesOf(many, false);
    expect(nodes).toHaveLength(7);
    expect(nodes[6].host).toBe("+6 more");
    expect(nodes[6].findings).toBe(6);
  });

  it("leaves the gate its room when a fetch was refused", () => {
    const rows = [live({}), live({ url: "https://b.example.com/" })];
    const open = nodesOf(rows, false);
    const gated = nodesOf(rows, true);
    expect(gated[1].x).toBeLessThan(open[1].x);
  });
});

describe("captionOf", () => {
  it("says only what the band drew", () => {
    expect(captionOf("working", 3, 12, 2, 1)).toBe(
      "3 hosts, 12 fetches since the last poll, 2 refused, 1 finding.",
    );
    expect(captionOf("idle", 1, 0, 0, 0)).toBe("1 host.");
    expect(captionOf("quiet", 0, 0, 0, 0)).toBe("Nothing is running.");
  });

  it("names the state that parked the scout", () => {
    expect(captionOf("waiting", 1, 0, 0, 0)).toContain("wants a person");
    expect(captionOf("held", 1, 0, 0, 0)).toContain("holds the wheel");
  });
});
