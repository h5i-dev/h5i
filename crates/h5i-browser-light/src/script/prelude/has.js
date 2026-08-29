// `:has()`, evaluated here because the engine's selector parser refuses it.
//
// Its own source, parsed the first time a selector containing `:has(` reaches
// `withHasMarkers` — which for most pages is never. The trigger is exact: the
// core already tests every selector against `HAS_PATTERN` to decide whether any
// of this is needed, so the test that used to choose between two code paths now
// chooses whether there is a second path yet.
//
// See `TIERS` in `mod.rs` for the rule, and the core's `withHasMarkers` for
// what calls into this.
(function () {
  "use strict";

  const api = globalThis.__h5i;
  const internals = globalThis.__h5iInternals;
  const { wrap, HAS_PATTERN } = internals;

  function rawAddClass(id, cls) {
    const original = api.getAttr(id, "class");
    api.setAttr(id, "class", original ? `${original} ${cls}` : cls);
    return () => {
      if (original === null) api.removeAttr(id, "class");
      else api.setAttr(id, "class", original);
    };
  }

  function splitTopLevelCommas(text) {
    const parts = [];
    let depth = 0;
    let current = "";
    for (const ch of text) {
      if (ch === "(" || ch === "[") depth += 1;
      else if (ch === ")" || ch === "]") depth -= 1;
      if (ch === "," && depth === 0) {
        parts.push(current);
        current = "";
      } else current += ch;
    }
    parts.push(current);
    return parts;
  }

  /// Split one complex selector into `[combinator, compound]` pairs at the
  /// top level: `"> p b"` becomes `[[">", "p"], [" ", "b"]]`. Brackets and
  /// parens shield their contents, so `a[title="> x"]` stays one compound.
  function splitRelativePairs(arg) {
    const pairs = [];
    let combinator = " ";
    let current = "";
    let depth = 0;
    const flush = () => {
      if (current !== "") {
        pairs.push([combinator, current]);
        current = "";
        combinator = " ";
      }
    };
    for (const ch of arg) {
      if (ch === "(" || ch === "[") depth += 1;
      else if (ch === ")" || ch === "]") depth -= 1;
      if (depth === 0 && (ch === ">" || ch === "+" || ch === "~")) {
        flush();
        combinator = ch;
      } else if (depth === 0 && /\s/.test(ch)) {
        flush();
      } else {
        current += ch;
      }
    }
    flush();
    return pairs;
  }

  /// Does `el` have a relative match for the pair chain? Evaluated locally —
  /// children for `>`, the sibling walk for `+`/`~`, a scoped query for the
  /// descendant hop — so the cost is the neighbourhood actually inspected,
  /// not a document-wide probe per candidate (which measured 244ms against
  /// this version's ~1ms on a 2,000-node page).
  function relativePairsHold(el, pairs, index) {
    if (index >= pairs.length) return true;
    const [combinator, compound] = pairs[index];
    const step = (candidate) =>
      candidate.matches(compound) && relativePairsHold(candidate, pairs, index + 1);
    if (combinator === ">") {
      for (const child of el.children) if (step(child)) return true;
      return false;
    }
    if (combinator === "+") {
      const next = el.nextElementSibling;
      return next !== null && step(next);
    }
    if (combinator === "~") {
      for (let n = el.nextElementSibling; n; n = n.nextElementSibling) {
        if (step(n)) return true;
      }
      return false;
    }
    for (const found of el.querySelectorAll(compound)) {
      if (relativePairsHold(found, pairs, index + 1)) return true;
    }
    return false;
  }

  /// Rewrite every `:has(ARG)` in `text` to a marker class, tagging the
  /// elements that match. Returns the rewritten selector and the cleanup
  /// that removes every marker.
  function prepareHasSelector(text) {
    const cleanups = [];
    const cleanup = () => {
      for (const undo of cleanups) undo();
    };
    try {
      let out = "";
      let i = 0;
      let group = 0;
      const lower = text.toLowerCase();
      while (i < text.length) {
        const at = lower.indexOf(":has(", i);
        if (at === -1) {
          out += text.slice(i);
          break;
        }
        out += text.slice(i, at);
        let depth = 1;
        let j = at + 5;
        while (j < text.length && depth > 0) {
          if (text[j] === "(") depth += 1;
          else if (text[j] === ")") depth -= 1;
          j += 1;
        }
        if (depth !== 0) {
          throw new DOMException(`${text} is not a valid selector`, "SyntaxError");
        }
        const argText = text.slice(at + 5, j - 1);
        if (HAS_PATTERN.test(argText)) {
          // `:has()` may not nest, per the spec's own grammar.
          throw new DOMException(`${text} is not a valid selector`, "SyntaxError");
        }
        const args = splitTopLevelCommas(argText).map((s) => s.trim()).filter(Boolean);
        if (args.length === 0) {
          throw new DOMException(`${text} is not a valid selector`, "SyntaxError");
        }
        // Validate each argument once, as the complex selector it is.
        for (const arg of args) {
          if (!api.validSelector(arg.replace(/^[>+~]\s*/, ""))) {
            throw new DOMException(`${text} is not a valid selector`, "SyntaxError");
          }
        }
        const marker = `__h5i_has_${group}__`;
        group += 1;
        const tagged = new Set();
        for (const arg of args) {
          if (/^[>+~]/.test(arg)) {
            // A leading combinator is evaluated *from the matches inward*:
            // one engine query finds every element matching the first
            // compound, the rest of the chain is verified locally from each,
            // and the anchor falls out of the combinator — the parent for
            // `>`, the previous sibling for `+`, every preceding sibling for
            // `~`. No per-candidate document scan: this is what took the
            // worst case from 244ms to ~2ms on a 2,000-node page.
            const pairs = splitRelativePairs(arg);
            const [combinator, first] = pairs[0];
            for (const id of api.queryAll(first, 0)) {
              const m = wrap(id);
              if (!m || !relativePairsHold(m, pairs, 1)) continue;
              if (combinator === ">") {
                const parent = api.parent(id);
                if (parent !== null && parent !== undefined) tagged.add(parent);
              } else if (combinator === "+") {
                const previous = m.previousElementSibling;
                if (previous) tagged.add(previous._id);
              } else {
                for (let n = m.previousElementSibling; n; n = n.previousElementSibling) {
                  tagged.add(n._id);
                }
              }
            }
          } else {
            // The descendant form has a fast path: every strict ancestor of
            // a match "has" it, because a scoped query only constrains the
            // subject to the scope's subtree.
            for (const id of api.queryAll(arg, 0)) {
              for (let p = api.parent(id); p !== null && p !== undefined; p = api.parent(p)) {
                tagged.add(p);
              }
            }
          }
        }
        for (const id of tagged) {
          if (api.nodeKind(id) === 1) cleanups.push(rawAddClass(id, marker));
        }
        out += `.${marker}`;
        i = j;
      }
      return { rewritten: out, cleanup };
    } catch (error) {
      cleanup();
      throw error;
    }
  }

  internals.prepareHasSelector = prepareHasSelector;
})();
