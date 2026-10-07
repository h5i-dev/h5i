// A small tokenizer for the Rust and Lean shown in the Apps console. It colors
// what a reader scans for (keywords, types, strings, comments) and leaves the
// rest plain; it is not a parser and never decides what code means.

export type Lang = "rust" | "lean" | "plain";
export type Kind =
  | "comment"
  | "string"
  | "number"
  | "keyword"
  | "control"
  | "type"
  | "fn"
  | "macro"
  | "attr"
  | "tactic"
  | "lifetime"
  | "op";
export type Token = { text: string; kind?: Kind };

const RUST_KEYWORDS = new Set(
  "as async await const crate dyn enum extern false fn impl in let mod move mut pub ref self Self static struct super trait true type union unsafe use where".split(
    " ",
  ),
);
const RUST_CONTROL = new Set(
  "break continue else for if loop match return while yield".split(" "),
);
const RUST_TYPES = new Set(
  "bool char str u8 u16 u32 u64 u128 usize i8 i16 i32 i64 i128 isize f32 f64".split(
    " ",
  ),
);

const LEAN_KEYWORDS = new Set(
  "theorem lemma def abbrev example axiom structure inductive class instance where deriving namespace section end open import variable universe noncomputable private protected partial mutual set_option attribute local scoped fun λ let have show from at with in by calc termination_by decreasing_by Type Prop Sort true false".split(
    " ",
  ),
);
const LEAN_CONTROL = new Set(
  "if then else match do return for unless".split(" "),
);
const LEAN_TACTICS = new Set(
  "simp simp_all simpa omega rfl exact intro intros cases rcases obtain induction rw rwa apply refine constructor decide unfold norm_num linarith nlinarith positivity exists use left right contradiction exfalso trivial assumption aesop split_ifs split ext funext congr subst specialize generalize revert clear repeat first try all_goals any_goals sorry native_decide bv_decide grind".split(
    " ",
  ),
);

export function langOf(path: string): Lang {
  if (path.endsWith(".rs")) return "rust";
  if (path.endsWith(".lean")) return "lean";
  return "plain";
}

/** Tokens of `code`, regrouped one array per source line. */
export function highlightLines(code: string, lang: Lang): Token[][] {
  const lines: Token[][] = [[]];
  for (const t of tokenize(code, lang)) {
    const parts = t.text.split("\n");
    parts.forEach((text, i) => {
      if (i > 0) lines.push([]);
      if (text) lines[lines.length - 1].push({ text, kind: t.kind });
    });
  }
  return lines;
}

export function tokenize(code: string, lang: Lang): Token[] {
  if (lang === "plain") return [{ text: code }];
  const out: Token[] = [];
  let i = 0;
  let plain = "";
  const push = (text: string, kind?: Kind) => {
    if (!kind) {
      plain += text;
      return;
    }
    if (plain) out.push({ text: plain });
    plain = "";
    out.push({ text, kind });
  };
  const rust = lang === "rust";
  let last = "";
  const lineComment = rust ? "//" : "--";
  const [open, close] = rust ? ["/*", "*/"] : ["/-", "-/"];

  while (i < code.length) {
    const c = code[i];
    const rest = code.startsWith.bind(code);
    if (rest(lineComment, i)) {
      const end = code.indexOf("\n", i);
      const j = end < 0 ? code.length : end;
      push(code.slice(i, j), "comment");
      i = j;
    } else if (rest(open, i)) {
      // Both languages nest block comments.
      let depth = 0;
      let j = i;
      while (j < code.length) {
        if (code.startsWith(open, j)) {
          depth++;
          j += 2;
        } else if (code.startsWith(close, j)) {
          depth--;
          j += 2;
          if (depth === 0) break;
        } else j++;
      }
      push(code.slice(i, j), "comment");
      i = j;
    } else if (rust && /^b?r#*"/.test(code.slice(i, i + 8))) {
      const m = /^b?r(#*)"/.exec(code.slice(i, i + 8))!;
      const fence = '"' + m[1];
      const end = code.indexOf(fence, i + m[0].length);
      const j = end < 0 ? code.length : end + fence.length;
      push(code.slice(i, j), "string");
      i = j;
    } else if (c === '"' || (rust && c === "b" && code[i + 1] === '"')) {
      let j = i + (c === "b" ? 2 : 1);
      while (j < code.length && code[j] !== '"') j += code[j] === "\\" ? 2 : 1;
      j = Math.min(j + 1, code.length);
      push(code.slice(i, j), "string");
      i = j;
    } else if (c === "'") {
      const ch = /^'(\\.[^']*|[^'\\\n])'/.exec(code.slice(i, i + 12));
      if (ch) {
        push(ch[0], "string");
        i += ch[0].length;
      } else if (rust && /^'[A-Za-z_]\w*/.test(code.slice(i))) {
        const m = /^'[A-Za-z_]\w*/.exec(code.slice(i))!;
        push(m[0], "lifetime");
        i += m[0].length;
      } else {
        push(c);
        i++;
      }
    } else if (/[0-9]/.test(c) && !/[\w.]/.test(code[i - 1] ?? " ")) {
      const m =
        /^(0x[0-9a-fA-F_]+|0b[01_]+|0o[0-7_]+|[0-9][0-9_]*(\.[0-9][0-9_]*)?([eE][+-]?[0-9]+)?)([a-z][a-z0-9]*)?/.exec(
          code.slice(i),
        )!;
      push(m[0], "number");
      i += m[0].length;
    } else if (rust && c === "#" && /^#!?\[/.test(code.slice(i, i + 3))) {
      const j = bracketEnd(code, code.indexOf("[", i));
      push(code.slice(i, j), "attr");
      i = j;
    } else if (!rust && c === "@" && code[i + 1] === "[") {
      const j = bracketEnd(code, i + 1);
      push(code.slice(i, j), "attr");
      i = j;
    } else if (/[A-Za-z_À-ɏͰ-Ͽ]/.test(c)) {
      const m = (
        rust ? /^[A-Za-z_][\w]*/ : /^[A-Za-z_À-ɏͰ-Ͽ][\w'!?À-ɏͰ-Ͽ₀-₉]*/
      ).exec(code.slice(i))!;
      const w = m[0];
      const after = code.slice(i + w.length);
      i += w.length;
      const prev = last;
      last = w;
      if (rust) {
        if (after.startsWith("!") && !after.startsWith("!=")) {
          push(w + "!", "macro");
          i++;
        } else if (RUST_CONTROL.has(w)) push(w, "control");
        else if (RUST_KEYWORDS.has(w)) push(w, "keyword");
        else if (RUST_TYPES.has(w) || /^[A-Z]/.test(w)) push(w, "type");
        else if (prev === "fn" || /^\s*(::\s*<[^>]*>\s*)?\(/.test(after))
          push(w, "fn");
        else push(w);
      } else {
        if (LEAN_CONTROL.has(w)) push(w, "control");
        else if (LEAN_KEYWORDS.has(w)) push(w, "keyword");
        else if (LEAN_TACTICS.has(w)) push(w, "tactic");
        else if (/^[A-Z]/.test(w)) push(w, "type");
        else push(w);
      }
    } else if (!rust && /[∀∃→←↔∧∨¬≠≤≥∈∉⟨⟩▸·∘×⁻¹]/.test(c)) {
      push(c, "op");
      i++;
    } else if (!rust && c === ":" && code[i + 1] === "=") {
      push(":=", "op");
      i += 2;
    } else {
      push(c);
      i++;
    }
  }
  if (plain) out.push({ text: plain });
  return out;
}

function bracketEnd(code: string, open: number): number {
  let depth = 0;
  for (let j = open; j < code.length; j++) {
    if (code[j] === "[") depth++;
    else if (code[j] === "]" && --depth === 0) return j + 1;
  }
  return code.length;
}
