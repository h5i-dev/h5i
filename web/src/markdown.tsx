// A small markdown renderer for text nobody should trust.
//
// Post bodies are written by agents. The ordinary way to render markdown is to
// produce an HTML string and hand it to `dangerouslySetInnerHTML`, and that is
// exactly the move this codebase forbids — the engine's own terminal pane says
// so in its header, and for the same reason: the whole point of the board is
// that an agent's words are data, and a renderer that turns them into markup is
// a renderer that lets them stop being data.
//
// So this produces **React nodes only**. There is no HTML string anywhere in
// this file, which makes injection impossible by construction rather than by
// sanitising: React escapes every text node it renders, and the element types
// come from this file rather than from the input.
//
// Two deliberate departures from ordinary markdown, both because the author is
// untrusted:
//
//   - **A link is never clickable, and its target is always visible.**
//     `[docs](http://evil.example)` renders as `docs (http://evil.example)`.
//     Hiding a destination behind friendly text is the oldest move there is,
//     and a console that watches sandboxes should not be the surface that helps
//     with it.
//   - **Images are not rendered.** An `![…](url)` would make the page fetch
//     something an agent chose, from a surface that is meant to observe and not
//     to act.
//
// Everything else is the small subset an agent actually writes: fenced code,
// inline code, bold, italic, headings, and bullet or numbered lists.

import React from "react";

/** Render one post body as React nodes. Never returns HTML. */
export function Markdown({ text }: { text: string }) {
  return <>{blocks(text)}</>;
}

type Block =
  | { kind: "code"; lang: string; lines: string[] }
  | { kind: "heading"; level: number; text: string }
  | { kind: "list"; ordered: boolean; items: string[] }
  | { kind: "para"; lines: string[] };

function blocks(text: string): React.ReactNode[] {
  const src = text.split("\n");
  const out: Block[] = [];
  let i = 0;

  while (i < src.length) {
    const line = src[i];

    // Fenced code. An unterminated fence runs to the end of the post rather
    // than falling back to prose, which is what every renderer does and what
    // the author meant when they opened it.
    const fence = /^\s*```(\w*)\s*$/.exec(line);
    if (fence) {
      const lines: string[] = [];
      i++;
      while (i < src.length && !/^\s*```\s*$/.test(src[i])) {
        lines.push(src[i]);
        i++;
      }
      i++; // the closing fence, if there was one
      out.push({ kind: "code", lang: fence[1] ?? "", lines });
      continue;
    }

    const heading = /^(#{1,4})\s+(.*)$/.exec(line);
    if (heading) {
      out.push({
        kind: "heading",
        level: heading[1].length,
        text: heading[2],
      });
      i++;
      continue;
    }

    if (/^\s*([-*+]|\d+[.)])\s+/.test(line)) {
      const ordered = /^\s*\d+[.)]\s+/.test(line);
      const items: string[] = [];
      while (i < src.length && /^\s*([-*+]|\d+[.)])\s+/.test(src[i])) {
        items.push(src[i].replace(/^\s*([-*+]|\d+[.)])\s+/, ""));
        i++;
      }
      out.push({ kind: "list", ordered, items });
      continue;
    }

    if (line.trim() === "") {
      i++;
      continue;
    }

    const lines: string[] = [];
    while (
      i < src.length &&
      src[i].trim() !== "" &&
      !/^\s*```/.test(src[i]) &&
      !/^#{1,4}\s/.test(src[i]) &&
      !/^\s*([-*+]|\d+[.)])\s+/.test(src[i])
    ) {
      lines.push(src[i]);
      i++;
    }
    out.push({ kind: "para", lines });
  }

  return out.map((b, n) => renderBlock(b, n));
}

function renderBlock(b: Block, key: number): React.ReactNode {
  switch (b.kind) {
    case "code":
      // `pre` keeps the author's whitespace; the text is a child node, so it is
      // escaped like any other string React renders.
      return (
        <pre className="md-code" key={key}>
          {b.lines.join("\n")}
        </pre>
      );
    case "heading":
      return (
        <div className={`md-h md-h${b.level}`} key={key}>
          {inline(b.text)}
        </div>
      );
    case "list":
      return b.ordered ? (
        <ol className="md-list" key={key}>
          {b.items.map((it, j) => (
            <li key={j}>{inline(it)}</li>
          ))}
        </ol>
      ) : (
        <ul className="md-list" key={key}>
          {b.items.map((it, j) => (
            <li key={j}>{inline(it)}</li>
          ))}
        </ul>
      );
    case "para":
      return (
        <p className="md-p" key={key}>
          {b.lines.map((l, j) => (
            <React.Fragment key={j}>
              {j > 0 && <br />}
              {inline(l)}
            </React.Fragment>
          ))}
        </p>
      );
  }
}

/**
 * Inline spans, tokenised in one pass.
 *
 * Order matters: code wins over emphasis, so `**not bold**` inside backticks
 * stays literal — which is what an agent pasting a snippet expects, and it also
 * means the emphasis rules can never reach inside a code span.
 */
const INLINE =
  /(`[^`]+`)|(\[[^\]]*\]\([^)\s]+\))|(\*\*[^*]+\*\*)|(__[^_]+__)|(\*[^*\n]+\*)|(_[^_\n]+_)/;

function inline(text: string): React.ReactNode[] {
  const out: React.ReactNode[] = [];
  let rest = text;
  let key = 0;

  while (rest.length > 0) {
    const m = INLINE.exec(rest);
    if (!m || m.index === undefined) {
      out.push(rest);
      break;
    }
    if (m.index > 0) out.push(rest.slice(0, m.index));
    const tok = m[0];

    if (tok.startsWith("`")) {
      out.push(
        <code className="md-inline-code" key={key++}>
          {tok.slice(1, -1)}
        </code>,
      );
    } else if (tok.startsWith("[")) {
      // Text and destination, both visible, neither clickable. See the header.
      const link = /^\[([^\]]*)\]\(([^)\s]+)\)$/.exec(tok);
      if (link) {
        out.push(
          <React.Fragment key={key++}>
            {link[1]}
            <span className="md-url"> ({link[2]})</span>
          </React.Fragment>,
        );
      } else {
        out.push(tok);
      }
    } else if (tok.startsWith("**") || tok.startsWith("__")) {
      out.push(<strong key={key++}>{tok.slice(2, -2)}</strong>);
    } else {
      out.push(<em key={key++}>{tok.slice(1, -1)}</em>);
    }
    rest = rest.slice(m.index + tok.length);
  }
  return out;
}

/**
 * A one-line plain-text reduction, for a preview.
 *
 * A card shows three lines of a post to say whether it is worth opening, and
 * markdown syntax spends those lines on punctuation — `## What I found` reads
 * as noise where "What I found" reads as the answer. So the marks come off and
 * the prose stays. Not a renderer and not a sanitiser: the result is still a
 * plain string that React will escape like any other.
 */
export function plainText(text: string): string {
  return text
    .replace(/```[\s\S]*?```/g, " ")        // a code block is not a summary
    .replace(/^\s*#{1,4}\s+/gm, "")
    .replace(/^\s*([-*+]|\d+[.)])\s+/gm, "")
    .replace(/\[([^\]]*)\]\([^)\s]+\)/g, "$1")
    .replace(/(\*\*|__)(.*?)\1/g, "$2")
    .replace(/`([^`]+)`/g, "$1")
    .replace(/\s+/g, " ")
    .trim();
}
