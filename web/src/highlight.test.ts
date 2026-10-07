import { describe, expect, it } from "vitest";
import { highlightLines, tokenize, type Kind, type Lang } from "./highlight";

const kinds = (code: string, lang: Lang) =>
  tokenize(code, lang)
    .filter((t) => t.kind)
    .map((t): [string, Kind] => [t.text, t.kind!]);

describe("highlight", () => {
  it("keeps every character", () => {
    const code = `#[derive(Debug)]\nfn f<'a>(x: &'a str) -> u8 { /* a /* b */ c */ println!("{x}"); 'z' as u8 }`;
    expect(
      tokenize(code, "rust")
        .map((t) => t.text)
        .join(""),
    ).toBe(code);
  });

  it("colors Rust", () => {
    expect(
      kinds(
        `pub fn go<'a>(s: &'a str) -> Option<u8> { if x { vec![b'c', 0x1f] } else { r#"q"# } }`,
        "rust",
      ),
    ).toEqual([
      ["pub", "keyword"],
      ["fn", "keyword"],
      ["go", "fn"],
      ["'a", "lifetime"],
      ["'a", "lifetime"],
      ["str", "type"],
      ["Option", "type"],
      ["u8", "type"],
      ["if", "control"],
      ["vec!", "macro"],
      ["'c'", "string"],
      ["0x1f", "number"],
      ["else", "control"],
      ['r#"q"#', "string"],
    ]);
  });

  it("nests block comments and splits them across lines", () => {
    const lines = highlightLines("/- a /- b -/\nc -/ theorem t", "lean");
    expect(lines[0]).toEqual([{ text: "/- a /- b -/", kind: "comment" }]);
    expect(lines[1][0]).toEqual({ text: "c -/", kind: "comment" });
    expect(lines[1][lines[1].length - 2]).toEqual({
      text: "theorem",
      kind: "keyword",
    });
  });

  it("colors Lean", () => {
    expect(
      kinds(
        "@[simp] theorem foo' : ∀ n : Nat, n + 0 = n := by simp -- done",
        "lean",
      ),
    ).toEqual([
      ["@[simp]", "attr"],
      ["theorem", "keyword"],
      ["∀", "op"],
      ["Nat", "type"],
      ["0", "number"],
      [":=", "op"],
      ["by", "keyword"],
      ["simp", "tactic"],
      ["-- done", "comment"],
    ]);
  });

  it("leaves other files plain", () => {
    expect(tokenize("fn x", "plain")).toEqual([{ text: "fn x" }]);
  });
});
