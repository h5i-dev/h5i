// Tests for the forum's untrusted-text renderer.
//
// Two things are being pinned here. First, the security invariants the file's
// own header promises: the renderer emits React nodes and never HTML, links are
// never clickable, images never render — a regression in any of those is a hole
// in the one surface that exists to keep an agent's words as data. Second, the
// parsing added for the console's markdown: diff colouring, tables, task lists,
// admonitions, and the `#n` chips whose numbering has to agree with
// `h5i forum read` or a chip points at the wrong post.
//
// The pure `parse()` is asserted directly; everything that only exists once
// rendered is checked through `renderToStaticMarkup`, which turns the React
// tree into the exact string a browser would receive — so "contains no
// `<script>`" is a real claim about the output, not about the source.

import { renderToStaticMarkup } from "react-dom/server";
import { describe, it, expect } from "vitest";
import { Markdown, parse, plainText } from "./markdown";

const html = (text: string, posts = 0) =>
  renderToStaticMarkup(<Markdown text={text} posts={posts} />);

describe("security invariants", () => {
  it("escapes raw HTML rather than emitting it", () => {
    const out = html("<script>alert(1)</script>");
    expect(out).not.toContain("<script>");
    expect(out).toContain("&lt;script&gt;");
  });

  it("renders a link as visible, unclickable text with no href", () => {
    const out = html("[docs](http://evil.example)");
    expect(out).not.toContain("<a ");
    expect(out).not.toContain("href");
    expect(out).toContain("http://evil.example"); // the destination is shown
  });

  it("does not make a javascript: link clickable", () => {
    const out = html("[x](javascript:alert(1))");
    expect(out).not.toContain("<a ");
    expect(out).not.toContain("href");
  });

  it("never renders an image element", () => {
    const out = html("![logo](http://x.example/y.png)");
    expect(out).not.toContain("<img");
  });
});

describe("post-reference chips (#n)", () => {
  it("chips a number that names an existing post", () => {
    const out = html("see #3 for details", 5);
    expect(out).toContain('class="md-ref"');
    expect(out).toContain("#3");
  });

  it("leaves a number with no matching post as plain text", () => {
    const out = html("see #3", 2);
    expect(out).not.toContain("md-ref");
    expect(out).toContain("#3");
  });

  it("never chips when the thread count is zero", () => {
    expect(html("#1", 0)).not.toContain("md-ref");
  });

  it("does not chip #0", () => {
    expect(html("#0", 5)).not.toContain("md-ref");
  });

  it("does not chip a number glued to a word, e.g. a hex colour", () => {
    const out = html("colour #12ab34 here", 99);
    expect(out).not.toContain("md-ref");
    expect(out).toContain("#12ab34");
  });
});

describe("admonitions", () => {
  const adm = (text: string) => {
    const b = parse(text)[0];
    if (b.kind !== "admonition") throw new Error(`expected admonition, got ${b.kind}`);
    return b;
  };

  it("reads a [!FINDING] marker as a finding-toned callout", () => {
    const b = adm("> [!FINDING]\n> the race is here");
    expect(b.tone).toBe("finding");
    expect(b.label).toBe("FINDING");
    expect(b.lines).toEqual(["the race is here"]);
  });

  it("maps DECISION to its own tone", () => {
    expect(adm("> [!DECISION]\n> serialize refreshes").tone).toBe("decision");
  });

  it("maps GitHub CAUTION/WARNING to amber risk, never red", () => {
    expect(adm("> [!CAUTION]\n> x").tone).toBe("risk");
    expect(adm("> [!WARNING]\n> x").tone).toBe("risk");
  });

  it("assigns no admonition kind a red tone", () => {
    const kinds = [
      "FINDING", "NOTE", "RISK", "BLOCKED", "WARNING", "CAUTION",
      "DECISION", "IMPORTANT", "DONE", "ACK", "TIP", "MYSTERY",
    ];
    const allowed = new Set(["finding", "risk", "decision", "done", "note"]);
    for (const k of kinds) {
      const tone = adm(`> [!${k}]\n> body`).tone;
      expect(allowed.has(tone)).toBe(true);
      expect(tone).not.toBe("red");
    }
  });

  it("renders the label so colour never stands alone", () => {
    const out = html("> [!RISK]\n> two valid refresh tokens");
    expect(out).toContain("md-adm is-risk");
    expect(out).toContain("RISK");
    expect(out).toContain("two valid refresh tokens");
  });

  it("treats a plain blockquote as a quote, not a callout", () => {
    expect(parse("> just a quote")[0].kind).toBe("quote");
  });
});

describe("diff code blocks", () => {
  it("classes added, removed and hunk lines", () => {
    const out = html("```diff\n+added\n-removed\n@@ -1 +1 @@\n context\n```");
    expect(out).toContain("is-add");
    expect(out).toContain("is-del");
    expect(out).toContain("is-hunk");
  });

  it("classes the file header, not as an added/removed line", () => {
    const out = html("```diff\n--- a/x\n+++ b/x\n```");
    expect(out).toContain("is-file");
  });

  it("does not diff-class a plain code block", () => {
    const out = html("```\n+not a diff\n```");
    expect(out).not.toContain("md-diff");
  });
});

describe("code block controls", () => {
  it("offers a copy button on every code block", () => {
    expect(html("```\nx\n```")).toContain("copy");
  });

  it("collapses a long log and offers to expand it", () => {
    const long = "```\n" + Array.from({ length: 30 }, (_, i) => `line ${i}`).join("\n") + "\n```";
    const out = html(long);
    expect(out).toContain("more lines");
    expect(out).toContain("expand");
  });

  it("does not collapse a short block", () => {
    const out = html("```\na\nb\n```");
    expect(out).not.toContain("more lines");
    expect(out).not.toContain("expand");
  });
});

describe("tables", () => {
  it("parses header, alignment and rows", () => {
    const b = parse("| a | b |\n| :-- | --: |\n| 1 | 2 |")[0];
    if (b.kind !== "table") throw new Error(`expected table, got ${b.kind}`);
    expect(b.header).toEqual(["a", "b"]);
    expect(b.align).toEqual(["left", "right"]);
    expect(b.rows).toEqual([["1", "2"]]);
  });

  it("renders as a real table with alignment classes", () => {
    const out = html("| a | b |\n| - | -: |\n| 1 | 2 |");
    expect(out).toContain("md-table");
    expect(out).toContain("<th");
    expect(out).toContain("is-right");
  });

  it("does not mistake a bare --- rule for a table", () => {
    expect(parse("text\n---\nmore").every((b) => b.kind !== "table")).toBe(true);
  });
});

describe("task lists", () => {
  it("keeps the checkbox markers as item text when parsing", () => {
    const b = parse("- [ ] todo\n- [x] done")[0];
    if (b.kind !== "list") throw new Error(`expected list, got ${b.kind}`);
    expect(b.items).toEqual(["[ ] todo", "[x] done"]);
  });

  it("renders inert ticked and unticked glyphs", () => {
    const out = html("- [ ] todo\n- [x] done");
    expect(out).toContain("md-tasklist");
    expect(out).toContain("☐");
    expect(out).toContain("☑");
  });
});

describe("inline and fences", () => {
  it("lets code win over emphasis", () => {
    const out = html("`**x**`");
    expect(out).toContain("<code");
    expect(out).toContain("**x**");
    expect(out).not.toContain("<strong");
  });

  it("runs an unterminated fence to the end of the post", () => {
    const b = parse("```\nkeep\ngoing")[0];
    if (b.kind !== "code") throw new Error(`expected code, got ${b.kind}`);
    expect(b.lines).toEqual(["keep", "going"]);
  });
});

describe("plainText preview", () => {
  it("strips markdown down to prose", () => {
    const out = plainText(
      "# Title\n\n> [!RISK]\n> danger\n\n- [ ] task\n\n| a | b |\n| - | - |\n| 1 | 2 |",
    );
    expect(out).not.toContain("|");
    expect(out).not.toContain("[!");
    expect(out).not.toContain("[ ]");
    expect(out).not.toMatch(/(^|\s)#/);
    expect(out).toContain("Title");
    expect(out).toContain("danger");
    expect(out).toContain("task");
  });
});
