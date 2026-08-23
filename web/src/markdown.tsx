// A small markdown renderer for text nobody should trust.
//
// Post bodies are written by agents. The ordinary way to render markdown is to
// produce an HTML string and hand it to `dangerouslySetInnerHTML`, and that is
// exactly the move this codebase forbids — the engine's own terminal pane says
// so in its header, and for the same reason: the whole point of the forum is
// that an agent's words are data, and a renderer that turns them into markup is
// a renderer that lets them stop being data.
//
// So this produces **React nodes only**. There is no HTML string anywhere in
// this file, which makes injection impossible by construction rather than by
// sanitising: React escapes every text node it renders, and the element types
// come from this file rather than from the input.
//
// Deliberate departures from ordinary markdown, all because the author is
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
//   - **The only interactive elements point inward.** A copy button copies the
//     text you can already see; a `#12` post-reference scrolls this page to a
//     post that exists in this same thread; a collapse toggle hides lines. None
//     of them can be made to reach off the page or to act on a box.
//   - **A task checkbox is inert.** `- [x]` renders a ticked glyph, not a form
//     control — the tick is the author's claim about their own list, not a
//     switch the reader can throw.
//   - **A diff is never red.** On this surface filled red is a host refusal and
//     red text is brand or selection (see `forum.css`); a removed line is amber
//     instead, and the leading `-` carries the meaning so colour never carries
//     it alone.
//
// Everything else is the subset an agent actually writes: fenced code (with a
// diff mode), inline code, bold, italic, headings, bullet / numbered / task
// lists, tables, blockquotes and GitHub-style `[!KIND]` admonitions.

import React from "react";

/**
 * Render one post body as React nodes. Never returns HTML.
 *
 * `posts` is the number of numbered posts in the thread this body belongs to,
 * which is what turns a `#12` in the text into a chip: the parser only makes a
 * chip when the number names a post that exists, so a stray `#500` stays plain
 * text rather than becoming a link to nothing. The count must be the same one
 * `h5i forum read` prints against — vote posts excluded, in thread order — or a
 * chip would scroll to a post the author did not mean. Zero (the default) means
 * "no thread to point into", so nothing is ever chipped.
 */
export function Markdown({ text, posts = 0 }: { text: string; posts?: number }) {
  return <>{parse(text).map((b, n) => renderBlock(b, n, posts))}</>;
}

type Align = "none" | "left" | "center" | "right";

type Block =
  | { kind: "code"; lang: string; lines: string[] }
  | { kind: "heading"; level: number; text: string }
  | { kind: "list"; ordered: boolean; items: string[] }
  | { kind: "table"; header: string[]; align: Align[]; rows: string[][] }
  | { kind: "quote"; lines: string[] }
  | { kind: "admonition"; tone: string; label: string; icon: string; lines: string[] }
  | { kind: "para"; lines: string[] };

const BULLET = /^\s*([-*+]|\d+[.)])\s+/;

/** A `| --- | :--: |` separator: pipes, dashes, colons and space, and a dash. */
function isDelim(line: string): boolean {
  return /^[\s|:-]+$/.test(line) && line.includes("-") && line.includes("|");
}

/** A row that a delimiter on the next line turns into a table header. */
function isTableStart(src: string[], i: number): boolean {
  return (
    src[i].includes("|") &&
    src[i].trim() !== "" &&
    i + 1 < src.length &&
    isDelim(src[i + 1])
  );
}

function splitRow(line: string): string[] {
  return line
    .trim()
    .replace(/^\|/, "")
    .replace(/\|$/, "")
    .split("|")
    .map((c) => c.trim());
}

function cellAlign(cell: string): Align {
  const l = cell.startsWith(":");
  const r = cell.endsWith(":");
  if (l && r) return "center";
  if (r) return "right";
  if (l) return "left";
  return "none";
}

// Exported for tests: the pure string→blocks half of the renderer, where the
// parsing decisions live (fences, tables, admonitions, lists). Rendering to
// React nodes is tested through `Markdown` itself.
export function parse(text: string): Block[] {
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
      out.push({ kind: "heading", level: heading[1].length, text: heading[2] });
      i++;
      continue;
    }

    // Blockquote, and its special case the admonition. Consecutive `>` lines
    // are one quote; if its first line is a `[!KIND]` marker the quote is a
    // callout instead, coloured by the same vocabulary the post kinds use.
    if (/^\s*>/.test(line)) {
      const q: string[] = [];
      while (i < src.length && /^\s*>/.test(src[i])) {
        q.push(src[i].replace(/^\s*>\s?/, ""));
        i++;
      }
      const marker = /^\[!([A-Za-z_]+)\]\s*(.*)$/.exec(q[0] ?? "");
      if (marker) {
        const tone = admonition(marker[1]);
        const body = marker[2] ? [marker[2], ...q.slice(1)] : q.slice(1);
        out.push({ kind: "admonition", ...tone, lines: body });
      } else {
        out.push({ kind: "quote", lines: q });
      }
      continue;
    }

    if (BULLET.test(line)) {
      const ordered = /^\s*\d+[.)]\s+/.test(line);
      const items: string[] = [];
      while (i < src.length && BULLET.test(src[i])) {
        items.push(src[i].replace(BULLET, ""));
        i++;
      }
      out.push({ kind: "list", ordered, items });
      continue;
    }

    if (isTableStart(src, i)) {
      const header = splitRow(src[i]);
      const align = splitRow(src[i + 1]).map(cellAlign);
      i += 2;
      const rows: string[][] = [];
      while (i < src.length && src[i].includes("|") && src[i].trim() !== "") {
        const cells = splitRow(src[i]);
        while (cells.length < header.length) cells.push("");
        rows.push(cells.slice(0, header.length));
        i++;
      }
      out.push({ kind: "table", header, align, rows });
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
      !/^\s*>/.test(src[i]) &&
      !BULLET.test(src[i]) &&
      !isTableStart(src, i)
    ) {
      lines.push(src[i]);
      i++;
    }
    out.push({ kind: "para", lines });
  }

  return out;
}

/**
 * A kind marker's tone, label and icon.
 *
 * The tones are the forum's own vocabulary — the same amber a RISK badge wears,
 * the same green a DONE wears — extended with cyan for findings and violet for
 * decisions so an inline callout and a post kind never disagree about what a
 * colour means. GitHub's five standard markers map onto the same tones so a
 * post reads the same here and there. Nothing maps to red: red on this surface
 * is the host's, not an agent's.
 */
function admonition(kind: string): { tone: string; label: string; icon: string } {
  const k = kind.toUpperCase();
  switch (k) {
    case "FINDING":
      return { tone: "finding", label: k, icon: "◆" };
    case "NOTE":
      return { tone: "finding", label: k, icon: "•" };
    case "RISK":
    case "WARNING":
    case "CAUTION":
      return { tone: "risk", label: k, icon: "▲" };
    case "BLOCKED":
      return { tone: "risk", label: k, icon: "■" };
    case "DECISION":
    case "IMPORTANT":
      return { tone: "decision", label: k, icon: "◈" };
    case "DONE":
    case "ACK":
    case "TIP":
      return { tone: "done", label: k, icon: "✓" };
    default:
      return { tone: "note", label: k, icon: "•" };
  }
}

/** A `- [ ]` / `- [x]` task item, split into its state and its text. */
function taskOf(item: string): { checked: boolean; text: string } | null {
  const m = /^\[([ xX])\]\s+(.*)$/.exec(item);
  return m ? { checked: m[1] !== " ", text: m[2] } : null;
}

function renderBlock(b: Block, key: number, posts: number): React.ReactNode {
  switch (b.kind) {
    case "code":
      return <CodeBlock lang={b.lang} lines={b.lines} key={key} />;
    case "heading":
      return (
        <div className={`md-h md-h${b.level}`} key={key}>
          {inline(b.text, posts)}
        </div>
      );
    case "list": {
      const hasTask = b.items.some((it) => taskOf(it));
      const cls = `md-list${hasTask ? " md-tasklist" : ""}`;
      const items = b.items.map((it, j) => {
        const task = taskOf(it);
        return (
          <li className={task ? "md-taskitem" : undefined} key={j}>
            {task && (
              <span className="md-check" aria-hidden="true">
                {task.checked ? "☑" : "☐"}
              </span>
            )}
            {inline(task ? task.text : it, posts)}
          </li>
        );
      });
      return b.ordered ? (
        <ol className={cls} key={key}>
          {items}
        </ol>
      ) : (
        <ul className={cls} key={key}>
          {items}
        </ul>
      );
    }
    case "table":
      return (
        <div className="md-tablewrap" key={key}>
          <table className="md-table">
            <thead>
              <tr>
                {b.header.map((c, j) => (
                  <th className={`is-${b.align[j] ?? "none"}`} key={j}>
                    {inline(c, posts)}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {b.rows.map((row, r) => (
                <tr key={r}>
                  {row.map((c, j) => (
                    <td className={`is-${b.align[j] ?? "none"}`} key={j}>
                      {inline(c, posts)}
                    </td>
                  ))}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      );
    case "quote":
      return (
        <blockquote className="md-quote" key={key}>
          {parse(b.lines.join("\n")).map((c, j) => renderBlock(c, j, posts))}
        </blockquote>
      );
    case "admonition":
      return (
        <div className={`md-adm is-${b.tone}`} key={key}>
          <div className="md-adm-head">
            <span className="md-adm-icon" aria-hidden="true">
              {b.icon}
            </span>
            <span className="md-adm-label">{b.label}</span>
          </div>
          <div className="md-adm-body">
            {parse(b.lines.join("\n")).map((c, j) => renderBlock(c, j, posts))}
          </div>
        </div>
      );
    case "para":
      return (
        <p className="md-p" key={key}>
          {b.lines.map((l, j) => (
            <React.Fragment key={j}>
              {j > 0 && <br />}
              {inline(l, posts)}
            </React.Fragment>
          ))}
        </p>
      );
  }
}

/**
 * A fenced code block: the language it declared, a copy of exactly its text,
 * and — for a long one — a collapse so a hundred-line log does not bury the
 * conversation. In `diff` mode each line is coloured by its first character;
 * everywhere else the text is one child node, escaped like any other string.
 */
const LONG = 24; // a log longer than this collapses by default
const PEEK = 10; // lines left showing while it is collapsed

function CodeBlock({ lang, lines }: { lang: string; lines: string[] }) {
  const [open, setOpen] = React.useState(false);
  const [copied, setCopied] = React.useState(false);
  const long = lines.length > LONG;
  const shown = long && !open ? lines.slice(0, PEEK) : lines;
  const isDiff = lang === "diff" || lang === "patch";

  const copy = () => {
    void navigator.clipboard?.writeText(lines.join("\n"));
    setCopied(true);
    setTimeout(() => setCopied(false), 1200);
  };

  return (
    <div className="md-codewrap">
      <div className="md-code-bar">
        {lang && <span className="md-code-lang">{lang}</span>}
        <span className="md-code-lines">
          {lines.length} {lines.length === 1 ? "line" : "lines"}
        </span>
        <span className="md-code-actions">
          {long && (
            <button
              type="button"
              className="md-code-btn"
              onClick={() => setOpen((o) => !o)}
            >
              {open ? "collapse" : "expand"}
            </button>
          )}
          <button type="button" className="md-code-btn" title="copy" onClick={copy}>
            {copied ? "copied" : "copy"}
          </button>
        </span>
      </div>
      <pre className={`md-code${isDiff ? " is-diff" : ""}`}>
        {isDiff
          ? shown.map((l, j) => (
              <span className={`md-diff ${diffClass(l)}`} key={j}>
                {l === "" ? " " : l}
              </span>
            ))
          : shown.join("\n")}
      </pre>
      {long && !open && (
        <button
          type="button"
          className="md-code-more"
          onClick={() => setOpen(true)}
        >
          … {lines.length - PEEK} more lines
        </button>
      )}
    </div>
  );
}

function diffClass(line: string): string {
  if (line.startsWith("@@")) return "is-hunk";
  if (line.startsWith("+++") || line.startsWith("---")) return "is-file";
  if (line.startsWith("+")) return "is-add";
  if (line.startsWith("-")) return "is-del";
  return "";
}

/** Scroll this page to a numbered post and flash it. Points inward only. */
function scrollToPost(n: number): void {
  const el = document.getElementById(`brd-post-${n}`);
  if (!el) return;
  el.scrollIntoView({ behavior: "smooth", block: "center" });
  el.classList.add("brd-post-flash");
  setTimeout(() => el.classList.remove("brd-post-flash"), 1200);
}

/**
 * Inline spans, tokenised in one pass.
 *
 * Order matters: code wins over emphasis, so `**not bold**` inside backticks
 * stays literal — which is what an agent pasting a snippet expects, and it also
 * means the emphasis rules can never reach inside a code span. A `#12` becomes
 * a chip only when it names a post that exists; `#12` inside `#12ab34` never
 * matches, because the number has to end on a word boundary.
 */
const INLINE =
  /(`[^`]+`)|(\[[^\]]*\]\([^)\s]+\))|(#\d+\b)|(\*\*[^*]+\*\*)|(__[^_]+__)|(\*[^*\n]+\*)|(_[^_\n]+_)/;

function inline(text: string, posts: number): React.ReactNode[] {
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
    } else if (tok.startsWith("#")) {
      // A reference to a numbered post, chipped only when the post exists — a
      // dead `#500` stays as the plain characters the author typed.
      const n = parseInt(tok.slice(1), 10);
      if (posts > 0 && n >= 1 && n <= posts) {
        out.push(
          <button
            type="button"
            className="md-ref"
            title={`go to post ${tok}`}
            onClick={() => scrollToPost(n)}
            key={key++}
          >
            {tok}
          </button>,
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
    .replace(/^\s*>\s?/gm, "")               // quote and admonition markers
    .replace(/^\s*\[![A-Za-z_]+\]\s*/gm, "")
    .replace(/^[\s|:-]+$/gm, " ")            // a table's delimiter row
    .replace(/\|/g, " ")                     // table cell walls
    // A separator, not nothing: joining list items end to end turns three
    // constraints into one run-on sentence that reads as a different claim.
    .replace(/^\s*([-*+]|\d+[.)])\s+/gm, " · ")
    .replace(/\[[ xX]\]\s+/g, "")            // a task checkbox
    .replace(/\[([^\]]*)\]\([^)\s]+\)/g, "$1")
    .replace(/(\*\*|__)(.*?)\1/g, "$2")
    .replace(/`([^`]+)`/g, "$1")
    .replace(/\s+/g, " ")
    .trim();
}
