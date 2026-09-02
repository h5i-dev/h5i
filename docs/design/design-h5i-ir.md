# Design: the Read IR

> A compact, read-only semantic tree between the Blitz DOM and everything an
> agent reads, so the cost of a step tracks what the step changed rather than
> how big the page is.

Status: draft 0.2. Phases 0 and 1 are built and measured. Target:
`crates/h5i-browser`. Surfaces: `snapshot`, `snapshot --delta`,
`markdown`, and action ref resolution. Paths are relative to its `src/`.

See [`design-browser.md`](design-browser.md) for the engine. This document
changes how agents read it, not what it loads or renders.

## In one screen

- The DOM stays the source of truth. The Read IR is a sparse semantic cache
  over it: roles, accessible names, visibility, state, targets. Nothing else.
- One walker feeds snapshot, delta and markdown, so the three views of a page
  stop disagreeing about what is hidden.
- `@ref` becomes a stable ID with an epoch and a tombstone, instead of a
  positional counter reset to `e1` on every capture.
- `snapshot --delta` becomes a revision-stamped change log. The quadratic LCS
  goes away. An unchanged delta costs O(1), not a full re-walk plus a table.
- Any mutation the engine cannot classify forces a full rebuild. The IR is
  never allowed to be stale and silent about it.
- Prior art is Chromium's accessibility abstraction as distilled by AccessKit.
  We take the design, not the dependency.
- Phases 0 and 1 reduced an agent's `snapshot --delta` step on a
  loaded page went from 1.43 ms and 11,844 allocations to 0.12 ms and 2,028,
  while cold `open` changed by about 1%. See [Measured](#measured).

## Why this exists

Originally every read and action walked the full Blitz DOM. Current status is
noted below; [Measured](#measured) has the results.

1. **Reads and actions both re-walk everything.** `Snapshot::capture`
   (`snapshot.rs`) runs for the `snapshot` verb, but also inside `click`,
   `type`, `set_checked`, `select` and `submit` (each verb takes a fresh
   internal capture to get a live node id before resolving the aim, see
   `resolve_aim` and `resolve_ref` in `stream.rs`), inside `hint_targets` and
   `text()` in `engine.rs`, and once per poll iteration inside `wait_for`.
   A `wait_for` that polls ten times walks the page ten times. *`text()` now
   reads through the IR; the rest still hold, and are phase 2's target.*

2. **Delta is quadratic and starts from scratch.** `Snapshot::delta` builds a
   `String` identity per line on both sides, then a dense
   `(n+1) x (m+1)` LCS table (`longest_common_subsequence` in `snapshot.rs`).
   The common case in an agent loop, "nothing changed", pays for a full
   capture, two identity vectors and the whole table. *Fixed for the unchanged
   case: `Snapshot::delta` now answers it directly. A page that really moved
   still builds the table.*

3. **Refs are positional and disposable.** `Walker.next_ref` starts at 1 on
   every capture; an insertion near the top of the page renumbers everything
   below it. The delta code already works around this: `line_identity`
   deliberately excludes the ref, and its comment says why. Action staleness
   is caught only by `same_target` in `stream.rs` comparing the old
   `RefEntry` against a fresh capture field by field. That check cannot tell
   "the same button, renumbered" from "a different button that happens to
   match", and it costs a capture per action to run.

4. **Name computation scans the document.** `labelled_by` iterates
   `doc.iter()` once per id in `aria-labelledby`, and `label_for` iterates the
   whole document looking for `label[for=id]` (`snapshot.rs`). A form page
   with many labelled controls goes quadratic in the number of nodes.

5. **The walk allocates per node.** `element.name.local.as_ref().to_string()`
   per element, `node.children.clone()` per recursion step, a `String` per role
   word, a fresh `String` from `collapse` per text node, `format!` per rendered
   field, and an owned four-string `Line` per output row collected into
   `Vec<Line>`. *The IR path has none of these; the walker still has all of
   them, and `Descriptor.role` is now the `ReadRole` enum both of them share.*

6. **Markdown is a second, divergent walker.** `markdown.rs` re-implements
   visibility as a bare `primary_styles` check (`is_displayed`). It has no
   `aria-hidden` handling, no closed-`details` handling, no in-frame special
   case, and drops `noscript` unconditionally where the snapshot honours the
   `scripted` flag. The two readings of one page can disagree today, and the
   `aria-hidden` gap is exactly the injection channel the snapshot walker
   closes (see the comment on `hidden_from_assistive_tech`).

The Read IR computes semantics once and updates only changed parts. Phase 1
removed per-node allocation and optimized unchanged deltas; the rest require a
retained tree.

## What it is not

- Not a replacement for the DOM, CSSOM, layout tree or paint tree. The DOM
  remains the substrate for script and for CSS-selector extraction.
- Not a full accessibility tree, and not a bridge to OS screen-reader APIs.
  If that ever becomes a goal, the right move is an adapter that converts the
  Read IR to AccessKit `TreeUpdate`s, not bending the internal representation
  to AccessKit's schema now.
- Not a complete ARIA/AAM implementation. v1 reproduces the current snapshot
  semantics byte for byte; new ARIA coverage is separate work, reviewed
  separately.
- Not lazy layout. Deferring style/layout/paint until a screenshot or
  geometry request ("semantic execution") is a plausible later win, but it
  requires splitting Blitz's resolve pipeline and is out of scope here. See
  [expected gains](#expected-gains-and-the-adoption-decision).
- Not a faster JavaScript engine. Boa costs what it costs.

## Prior art: Chromium and AccessKit

Chromium's accessibility stack has the same shape this design needs: a
Blink-side layer that computes role, name and hidden state from DOM and layout
(`ax_node_object.cc`, `ax_object_cache_impl.cc` with its dirty-object and
deferred-update machinery), and a generic layer (`ui/accessibility`) with a
compact node (`ax_node_data.h`), a tree, and an incremental serializer over
stable IDs (`ax_tree_serializer.h`). The Blink half is inseparable from
Blink's DOM, lifecycle and shadow-tree types; the generic half is portable
design.

AccessKit (`~/Ref/accesskit`, Apache-2.0 OR MIT, with significant portions
derived from Chromium under its BSD license, see its `LICENSE.chromium`) is
that portable half re-done in Rust, and it is worth copying from precisely:

- **Stable integer IDs.** `NodeId` is a `#[repr(transparent)]` u64, unique
  per tree, supplied by the provider and expected to be stable across updates.
- **A frozen node.** `Node` is `role: Role` (`#[repr(u8)]`, 182 variants
  ordered by expected frequency), two u32 action bitmasks, a u32 flag word for
  18 booleans, and a sparse property store.
- **Sparse properties via an index table.** `Properties` holds a dense
  `[u8; 87]` table mapping property id to a slot in a `Vec<PropertyValue>`,
  so only set properties occupy a value slot (the librsvg technique, credited
  in a comment in `accesskit/src/lib.rs`). Equality is defined through the
  table, so it is insertion-order independent. Large payloads are boxed.
- **Wholesale-node updates.** A `TreeUpdate` carries whole replacement nodes,
  never field patches; "unchanged fields must still be set to the same values
  as before". A full tree and a delta are the same message type.
- **Change detection by value equality**, in the consumer, over the
  order-insensitive `PartialEq`, with a double-buffered `TreeState` committed
  via `clone_from` to reuse allocations. No `Arc` or `Rc` anywhere on the hot
  path; readers get borrowed handles.
- **Invalid updates are fatal.** The consumer panics on orphan nodes,
  dangling child ids, duplicate children, a focus outside the tree. Nothing
  malformed is silently accepted.
- **What it will not do for us.** AccessKit computes nothing from HTML, CSS
  or ARIA; the application supplies finished nodes, roles and names. The
  DOM-to-semantics half, which is most of the work here, stays ours either
  way.

h5i has a small, fixed role and attribute vocabulary, so a purpose-built IR is
smaller than AccessKit's schema. It adopts stable ids, frozen sparse nodes,
wholesale updates, equality-based change detection, and fatal validation. OS
accessibility can later use an `ir -> TreeUpdate` adapter.

## Invariants

### Source of truth

- The DOM and computed style are authoritative. The Read IR never mutates
  either.
- Every action revalidates `@ref -> DOM node` before dispatching. The IR
  proposes; the DOM disposes.
- Arbitrary CSS extraction keeps using the DOM selector engine
  (`selector.rs`, `extract.rs`). The IR is lossy by design and must never be
  the substrate for a selector match.

### Consistency

- The IR is only published at a clean point: after the same
  `take_dirty()` / `lay_out` sequence the engine already runs after script
  (`after_script`, `dispatch_event`, `type_into` in `engine.rs`).
- While the document is dirty, the previous IR must not be served as current.
- Updates build in a shadow builder and commit atomically: tree, revision and
  change log switch together or not at all.
- A failed update surfaces as an error or an engine note. Silently serving
  the previous tree is forbidden; a stale reading that looks fresh is worse
  than no reading.

### Page-supplied data

- `name`, `text`, `value`, `target`, `description` are untrusted page content.
  They live in the page text arena and nowhere else.
- Trusted engine facts (notes, the resolved URL, truncation markers) never
  enter the arena. Only the renderer draws the fence, using the existing
  `CONTENT_BEGIN` / `CONTENT_END` / `UNTRUSTED_NOTE` machinery and the
  per-value `one_line` defang from `snapshot.rs`. The per-line invariant
  ("nothing page-supplied can begin a line") and the fixed, non-nonce fence
  markers are load-bearing security properties and carry over unchanged.
- Password values never enter the IR. The IR stores `PASSWORD_MASK`, exactly
  as `accessible_name` masks today. Credential placeholders resolved by the
  secrets path (`secrets.rs`) likewise never appear.

### Identity

- Internal `ReadId`s are generational: slot index plus generation, so a
  reference into a freed slot is detected, not misread.
- User-facing `RefId`s are monotonic within a document epoch and never
  reused. Deletion tombstones the ref.
- Navigation bumps the document epoch and invalidates every outstanding ref,
  which matches what `Session::land` already does by dropping `served_refs`
  and `hint_refs` (`stream.rs`).

## Lifecycle

### Transient mode

For one-shot reads: clean the document, build a temporary arena, stream output,
then drop it. This measures the compact representation without caching.

### Retained mode

For live `stream.rs` sessions. The first read builds lazily; later reads rebuild
dirty subtrees. Navigation or budget overrun triggers a full rebuild. Before
the first read, mutations only bump `document_revision`.

The engine has no revision counter today (the only related state is the
boolean `HostHandle.dirty` and `styles_stale` in `script/host.rs`, and the
viewer frame counter `Session.seq` in `stream.rs`). `document_epoch` and
`document_revision` are new fields on `Page`:

```rust
struct Page {
    doc: Dom,                       // existing Rc<RefCell<BaseDocument>>
    document_epoch: u64,            // bumped by Page::open / land
    document_revision: u64,         // bumped by every classified mutation
    read_cache: Option<ReadCache>,  // retained mode only
    // existing fields...
}
```

The `Dom` is an `Rc<RefCell<BaseDocument>>` and the whole engine is
single-threaded; the IR follows suit. No `Send`/`Sync`, no locks, no `Arc`.

## Data model

### ID types

```rust
#[repr(transparent)] struct ReadId(u32);    // slot index + generation
#[repr(transparent)] struct RefId(u32);     // monotonic within an epoch
#[repr(transparent)] struct DomNodeId(u32); // checked from Blitz's usize
#[repr(transparent)] struct AttrSetId(u32); // 0 = no attributes
#[repr(transparent)] struct TextId(u32);    // 0 = empty
```

Blitz node ids are `usize`. A document whose ids exceed u32 fails IR
construction explicitly; silent truncation is forbidden. (Such a document has
already blown every budget in this design.)

### Core node

```rust
#[repr(C)]
struct ReadNode {
    dom_id: DomNodeId,
    parent: ReadId,
    first_child: ReadId,
    next_sibling: ReadId,
    name: TextId,
    attrs: AttrSetId,
    role: ReadRole,          // u16
    flags: ReadFlags,        // u16
    local_revision: u32,
    subtree_fingerprint: u64,
}
```

Requirements:

- 48 bytes or under, enforced by a compile-time assertion. If a future field
  forces growth, the overrun is recorded with a benchmark, not waved through.
- Sibling links instead of a `Vec<child>` per node. The arena is the only
  allocation.
- No `String` anywhere in the node. Roles are the enum; text is a `TextId`.
- Optional data lives in the sparse attribute table, not in the node.
- The fingerprint is a fast-skip hint for delta only. Correctness always
  comes from exact comparison, never from a hash.

What phase 1 actually built is a subset of the above, and the differences are
deliberate rather than partial work:

- `local_revision` and `subtree_fingerprint` are absent. Nothing reads them
  until phase 3's incremental invalidation, and a field nothing reads is a
  field nothing tests. They are what the remaining budget is being held for:
  the shipped node is 28 bytes.
- The child links are absent too. The builder emits in preorder and stores a
  `depth`, which determines the same tree an indented outline does, and phase 1
  renders and diffs by walking that sequence flat. Child links buy O(1)
  navigation, which is a phase 3 need.
- `href` sits in the node rather than in the attribute table. It is printed on
  the same line as the name, every link has one, and a side allocation for the
  commonest actionable element on the web costs more than the four bytes it
  saves.
- `ReadId` carries no generation. Generations catch a reference into a freed
  slot, and while every tree is built and dropped whole there is no freed slot
  to catch. They arrive with the retained arena.
- One flag was added that the sketch did not have: `VERBATIM`, marking the
  lines of a code block, whose leading indentation the arena stores as-is
  while every other string arrives collapsed. Without it the renderer cannot
  tell which strings still need normalising, and the outline silently indents
  code the walker did not.

### Roles

```rust
#[repr(u16)]
enum ReadRole {
    Document, Text, Heading, Paragraph, Link, Button, TextInput,
    Checkbox, Radio, Combobox, Option, List, ListItem, Table, Row,
    ColumnHeader, RowHeader, Cell, Image, Code, Quote, Label,
    Separator, Landmark, Group,
}
```

Heading level, input kind and similar refinements are sparse attributes. The
renderer maps `Heading` plus level to the exact strings the outline prints
today (`heading1` through `heading6`, `paragraph`, `listitem`, `cell`,
`label`, `code`, `quote`, `link`, `button`, `combobox`, `textbox`, `image`,
`checkbox`, `radio`, `text`), because v1 output compatibility is byte-level.
Unknown elements do not become string roles; a container with nothing to say
is flattened, exactly as `describe` returning `None` flattens it today.

AccessKit orders its role enum by expected frequency for variable-length
encodings; ours is small enough not to care, but `Text` and the structural
roles go first on the same reasoning.

### Flags

Bitflags, minimum set: `ACTIONABLE`, `DISABLED`, `CHECKED`, `SELECTED`,
`EXPANDED`, `REQUIRED`, `EDITABLE`, `FOCUSED`, `MULTILINE`, `PASSWORD`,
`IN_FRAME`, `PRESENTATIONAL`.

`IN_FRAME` is not optional: the walker already carries an `in_frame` bit
through the whole grafted subtree because Blitz styles nothing inside a frame
graft, and visibility inside one is judged by the `hidden` attribute and
inline `display:none` instead of resolved style (`Walker::walk` in
`snapshot.rs`, and `load_frames` in `engine.rs` with its `MAX_FRAMES = 8`
cap). The IR must preserve that judgment or frames go dark.

There is no general `HIDDEN` flag. Hidden subtrees are not inserted into the
public tree at all. The one exception, a hidden element that feeds an
accessible name, is reachable through the name dependency index only, never
through the tree.

### Sparse attributes

```rust
struct ReadAttrs {
    description: TextId,
    value: TextId,
    target: TextId,       // resolved href/src, collapsed
    placeholder: TextId,
    ref_id: RefId,
    level_or_kind: u16,   // heading level, input kind, list kind
    state_value: i32,
}
```

Allocated only for nodes that need one; a plain paragraph or text node
carries `AttrSetId(0)`. Our attribute set is small and closed, so a fixed
struct beats AccessKit's index-table scheme for now. If the set grows past a
handful of optional fields, the librsvg-style index table is the named
fallback, not ad hoc `Option`s in the node.

### Text storage

```rust
struct TextEntry { chunk: u16, start: u32, len: u32 }
```

- Normalized text (`collapse`d, exactly as today) is appended to immutable
  chunk arenas; `TextTable[TextId]` resolves an entry.
- No global interner for natural-language text: page text has low duplication,
  hashing every string costs more than it saves, and an interner is a
  DoS-shaped unbounded growth vector on hostile pages. Tag and attribute
  names are already atoms upstream; roles are the enum.
- Long runs (preformatted blocks) may span multiple chunks.
- Incremental updates append; when dead bytes exceed 25% of a chunk or the
  text budget is hit, compact fully. Navigation frees all chunks at once.

## Tree construction

The build must reproduce the current walker's decisions exactly. They are
listed here because each one is a behaviour an agent depends on, with the
implementation in `Walker::walk` and its helpers in `snapshot.rs`:

Included: visible text (including loose text in anonymous containers);
headings, paragraphs, lists, tables, quotes, code; actionable elements (the
`role_for` table: links with an href, buttons, inputs by type with
`type=hidden` excluded, `select`, `textarea`, images); alt text; the
`summary` of a closed `details`; explicit ARIA roles from
`descriptor_for_aria_role`, which override the tag.

Excluded: `script`, `style`, `head`, `title`, `meta`, `link`; `noscript` when
this reading ran script (the `scripted` flag on capture, and the IR is built
per reading with that flag baked in); subtrees with `display: none` or no
resolved style outside a frame graft; `aria-hidden="true"` subtrees
(inherited, bounded by the same depth the walk is); the body of a closed
`details`; password and credential values; anything past `MAX_DEPTH = 24`.

Flattening: `describe` returning `None` (div, span, section, nav, main and
every unknown tag) produces no node; children attach to the nearest public
ancestor at the same depth, so the outline tracks meaning rather than markup
nesting. The prose-suppression rule carries over: under a semantic leaf whose
text was emitted, only actionable descendants get nodes. So does the
block-hoisting rule (`hoists_a_block` / `direct_text`): a leaf that swallowed
block structure keeps only its direct text and lets the blocks speak.

`<pre>` keeps per-source-line structure (one child text node per line, indent
preserved via `collapse_keeping_indent`), because the outline prints one line
per source line and markdown needs the raw newlines for its widened backtick
fence.

## Names

v1 computes exactly what `accessible_name` computes today, in the same
precedence order, including the password masking and the editor-over-attribute
rule for typed input values. No new ARIA in the same change.

What changes is the cost and the bookkeeping:

- The build pass constructs an `id -> DomNodeId` index and a
  `label[for] -> control` reverse index once. `labelled_by` and `label_for`
  become lookups instead of whole-document scans.
- Every computed name records its dependencies:

```rust
struct NameDependencies {
    source: DomNodeId,
    referenced_ids: SmallVec<[DomNodeId; 2]>,
    ancestor_scope: DomNodeId,
}
```

so a change to `aria-labelledby`, `aria-describedby`, `label[for]`, `alt`,
`title`, `placeholder`, an `id` attribute, or descendant text dirties exactly
the public nodes whose names depend on it.

## Refs and actions

```rust
struct RefEntry {
    ref_id: RefId,
    read_id: ReadId,
    dom_id: DomNodeId,
    document_epoch: u64,
}
```

Rules:

- A ref is minted the first time an actionable node is published, and kept as
  long as that DOM node stays in the public tree. Two captures of an
  unchanged page carry identical refs, which also restores refs to the
  delta's line identity (today `line_identity` must exclude them because the
  positional counter renumbers the page under any insertion).
- A removed node's ref becomes a tombstone. It is never reassigned within the
  epoch. An action against a tombstone fails as `stale ref`, never guesses.
- Action validation checks, in order: epoch, `ReadId` generation, DOM node
  existence, actionable status. This replaces the current `same_target`
  field-equality heuristic in `stream.rs` and removes the per-action full
  capture that feeds it.
- Navigation fails every outstanding ref, as `Session::land` effects today by
  dropping the served handle sets.
- The per-ref durable CSS selector the `snapshot` verb emits
  (`selector::Cache` in `stream.rs`) is unaffected; it reads the DOM.

## Mutation and invalidation

### Where mutations actually happen

The seams are known and finite, and the design names them because missing one
means a stale reading:

1. **Script.** Every tree mutation from the realm funnels through
   `guard_mutation` in `script/dom_api.rs` (append, insert, remove, setText,
   setAttribute, innerHTML). This is the attach point for classified
   invalidation; it already holds the host handle. The prelude's
   `MutationObserver` is a JS-side shim and observes only what the shim's own
   methods did; it is not an invalidation source.
2. **Native verbs.** `type_into`, `set_checked`, `select_option`, `focus` in
   `engine.rs` mutate the DOM directly and bypass `host.dirty` today. Each
   gets an explicit IR notification.
3. **Style.** `styles_stale` and stylesheet swaps invalidate the subtree
   style resolution reports; when the engine cannot bound it, the whole
   document is dirty.
4. **Frame grafts.** `load_frames` appends grafted subtrees during open;
   open always rebuilds, so no incremental path is needed there.
5. **Canvas** has its own dirty set (`Canvases::dirty`) and never changes the
   readable tree; it is out of scope.

### Revision state

```rust
struct ReadCache {
    document_epoch: u64,
    document_revision: u64,
    materialized_revision: u64,
    tree: ReadTree,
    dirty: DirtyQueue,
    changes: ChangeLog,
}
```

Mutations within one task batch into one revision bump.

### Dirty classification

| Mutation | Dirtied |
|---|---|
| text node change | the text node and every public ancestor whose name depends on it |
| local attribute change | that node |
| `id`, `for`, `aria-labelledby`, `aria-describedby` | every dependent via the relation index |
| `role`, `hidden`, `aria-hidden`, `inert`, `open` | that subtree |
| class/style/stylesheet change | the subtree style invalidation reports, else the document |
| insert / remove / move | old parent, new parent, the subtree |
| base URL change | every node holding a target |

### Safe fallback

Full rebuild, not a guess, whenever: the mutation cannot be classified; dirty
roots exceed their budget; the relation index is inconsistent; style and DOM
revisions disagree; the arena is past its fragmentation or size budget; or
the invariant check fails. Ignoring an unclassified mutation for speed is the
one optimization this design bans outright.

### Commit

1. Drain pending script/microtasks within the existing settle budget.
2. Bring style to the clean point the snapshot already requires.
3. Merge dirty roots upward (an ancestor subsumes its descendants).
4. Rebuild the dirty subtrees in a shadow builder.
5. Diff old and new nodes by exact field comparison into a change set.
6. Run tree invariant checks.
7. Commit tree, revision and change log atomically.

## Snapshot rendering

The renderer walks the IR preorder and writes straight into the output
buffer. No `Vec<Line>`, no per-line `String`s, no `format!` per field.

```rust
fn render_snapshot(
    tree: &ReadTree,
    out: &mut dyn std::fmt::Write,
    budget: OutputBudget,
) -> RenderResult;
```

Renderer responsibilities, unchanged in meaning from `Snapshot::render`:

- the trusted header (resolved URL, engine notes) outside the fence, the
  fence markers and `UNTRUSTED_NOTE`, the page title inside;
- depth indentation capped at `MAX_DEPTH`, the `- ` prefix, the fixed role
  strings, `one_line` normalization of every page-derived field, `[ref=eN]`
  and `-> target`;
- explicit truncation at the line budget (`max_snapshot_lines`, default 500
  in `PageOptions`), never silent;
- byte-identical output for the same revision. The fence's unforgeability
  argument depends on deterministic, diffable output and on no page-supplied
  value starting a line; both are properties the existing tests pin
  (`a_page_cannot_forge_the_end_of_the_fence` and neighbours in
  `snapshot.rs`) and those tests carry over as-is.

## Delta

The LCS dies. In its place, a revision-stamped change log:

```rust
enum ReadChange {
    Added(ReadId),
    Removed(RemovedNodeSummary),
    Updated { id: ReadId, fields: FieldMask },
    Moved { id: ReadId, old_parent: ReadId, new_parent: ReadId },
}
```

- `snapshot --delta` aggregates changes since the revision last served to
  that consumer (`last_snapshot` in `Session` becomes a revision, not a
  clone of the whole snapshot).
- "Unchanged" is decided by dirty processing plus exact comparison; the
  answer renders as today's `no change: this action did not alter the
  readable page` line.
- The `replaced` heuristic survives with the same threshold
  (`REPLACED_SURVIVAL = 0.25`) and the same rendered fallback: a navigation,
  an expired log, or a page that replaced most of itself returns the full
  outline with a stated reason, exactly as `stream.rs` does now.
- The change log is a bounded ring. A consumer whose revision fell off the
  end gets `replaced`, not an unbounded history.
- Complexity targets: unchanged delta O(1); incremental delta O(committed
  changes + output); auxiliary memory O(changes); full snapshot O(visible
  nodes + output).

The added/removed rendering keeps the current `- ` / `+ ` fenced form from
`Delta::render`.

## Markdown and extract

### Markdown

Phase 4 moves `markdown.rs` onto the IR: block and inline roles, heading
levels, list kind and start, table cells, link targets and code state all
come from the tree; markdown-specific escaping (the narrow `escape`, the
widened backtick fence, whole-document `defang_fence`) stays in the renderer.

This is the one phase that intentionally changes output: markdown inherits
the snapshot's `aria-hidden`, closed-`details`, in-frame and
`noscript`-vs-`scripted` judgments, which it lacks today. That is a
semantics fix (and closes a real injection channel), but it is reviewed as
its own change with its own fixtures, never smuggled inside an IR refactor.

### Extract

CSS-selector extraction is untouched. The IR may answer only what it stores:
visible text, actionable elements, role/name/state, targets and values.
Anything else (JSON-LD, OpenGraph, `head` metadata) keeps its own path
(`structured.rs`) against the DOM.

## Budgets and DoS resistance

All configurable; initial values:

| Budget | Initial |
|---|---|
| `max_read_nodes` | 50,000 |
| `max_read_text_bytes` | 8 MiB |
| `max_dirty_roots` | 1,024 |
| `max_change_log_bytes` | 2 MiB |
| `max_name_dependency_edges` | 100,000 |

On overrun: never abort the process; never pass a partial result off as
complete; always attach `truncated` plus a concrete engine note (the existing
note channel on `Page`); mint refs only for nodes actually published; and if
the relation-edge budget trips, do not trust the incomplete dependency index,
schedule a full rebuild instead.

These sit alongside the walk's existing self-defence: `MAX_DEPTH`,
`prune_deep_nesting` at parse, the line budget, and the bounded ancestor scan
in `hidden_from_assistive_tech`.

## Module layout

```
src/read_ir/
  mod.rs              *  the Snapshot compatibility seam
  model.rs            *  ids, nodes, roles, flags, size assertions
  text_arena.rs       *  immutable text storage
  build.rs            *  DOM + style -> ReadTree
  render_snapshot.rs  *  ReadTree -> the text an agent reads
  delta.rs            *  unchanged fast path; defers the rest to the walker
  equivalence.rs      *  the phase 1 acceptance gate (tests)
  names.rs               accessible name + dependency index
  refs.rs                stable @ref lifecycle, tombstones
  invalidate.rs          mutation classification
  commit.rs              safe-point update, atomic commit
  render_markdown.rs
  invariants.rs
```

`*` is what phase 1 built. `text_arena.rs` is one buffer and a span table
rather than the design's chunk list: chunking exists to free and compact
incrementally, which a tree that is dropped whole never does.

`snapshot.rs` keeps the fence constants, `collapse`, `one_line`,
`defang_fence` and the role tables through the transition; the IR build calls
into them so there is exactly one implementation of each judgment at every
point in the rollout.

## Testing

### Output compatibility

- Byte comparison of old and new renderers over the existing snapshot
  fixtures and the security tests (fence forging, terminal repaint, password
  mask, flush-left invariant).
- Any intended difference is reviewed on its own; an IR PR that changes
  semantics has failed review by definition.
- Snapshot and markdown agree on hidden judgment (from Phase 4 on).

`read_ir/equivalence.rs` is where this lives, and it compares four outputs, not
one: the rendered outline, the ref list, the materialised `Snapshot` and the
`truncated` flag. Four rather than one because the rendered outline is the
weakest of them. Rendering collapses every line on the way out, so a reading
can be wrong in the structured `Line.text` a delta serialises and identical in
the text an agent sees; that is exactly the bug the gate caught first.

Three layers, in increasing distrust of the author:

1. A hand-written corpus of the shapes the walk treats specially.
2. Those shapes crossed with both script modes and with budgets around the
   edges where truncation lands part way through a node, about eight hundred
   readings.
3. Randomly grown markup from a fixed seed, which tests the combinations
   nobody thought of. Reproducible by construction: a failure names its case.

### Mutation sequences

Text insert/remove/replace; node insert/remove/move; role and ARIA changes;
`display:none` toggles; `details` open/close; `label` and `aria-labelledby`
retargeting; frame content; post-navigation staleness; tombstoned refs never
resolving; typed input values updating through `type_into`.

### Property tests

No cycles through parent/child/sibling links; every live `ReadId` generation
matches its slot; every public ref points at a live actionable node;
tombstones never resolve; page text never renders outside the fence; and the
central one: for any mutation sequence, incremental update and full rebuild
produce equal trees.

### Performance

Fixtures: a large static document, a deeply nested DOM, a form-heavy page,
and a JS page applying small repeated mutations. Measured per phase: wall
time split across parse / script / resolve / IR build / render; full snapshot
latency; unchanged-delta latency; one-node-mutation delta latency; allocation
count and bytes; peak RSS; retained bytes per node. Regression thresholds go
into CI only after Phase 0 baselines exist.

## Acceptance criteria

Functional. The first is met and gated by `read_ir/equivalence.rs`, which reads
a corpus of the shapes the walk treats specially both ways and compares the
rendered outline, the ref list and the materialised snapshot; the rest belong to
the phases that introduce what they describe.

- *Met.* Existing snapshot tests pass byte-identically, approved diffs aside.
  The whole crate suite (817 tests) is green with the IR in the `--text` path.
- A stale ref never dispatches to a different node. (Phase 2.)
- Incremental and full rebuild agree on the final tree. (Phase 3.)
- Uncertainty always resolves to a full rebuild or an explicit error. (Phase 3.)

Structural:

- *Met.* No per-node heap allocation for roles or tags: the role is an enum and
  the tag is a borrowed `&str`.
- *Met.* No intermediate `Vec<Line>` in full snapshot rendering. Rendering a
  26 KB outline makes three allocations.
- *Met, for the case that pays.* No quadratic table in an unchanged delta,
  which now allocates once. A changed page still runs the walker's subsequence;
  phase 3 replaces it.
- An unchanged delta performs no DOM walk. (Phase 2: it needs a retained tree.
  Phase 1 still captures, then compares in O(lines) with no allocation.)
- *Met.* `ReadNode` is 48 bytes or the overrun is documented. It is 28, asserted
  at compile time.
- *Met.* Transient arenas are freed after output; nothing is retained.

**Structured readings and rendered outlines must both match.** Early arena
trimming preserved rendered output but lost code indentation in serialized
`Line.text`; rendered-byte tests alone missed it.

## Measured

Phases 0 and 1 have landed. `crates/h5i-browser/benches/read.rs` is the
instrument: median of 9 runs after a warm-up, allocations counted by a global
allocator armed around one call. Same box as the engine's other numbers
(aarch64, WSL2), release profile. The bench asserts the two readings are
byte-identical before it times them, so every row compares two ways of
producing the same bytes.

Absolute times move with what else the box is doing, by up to 40% between
runs. Ratios and allocation counts do not, and allocation counts are exact, so
those are what the claims below rest on.

The unchanged delta is measured between two *separately captured* readings of
one document rather than one reading compared with itself. Self-comparison
hands every string comparison two identical pointers, which is not the shape of
an agent's second snapshot and would let the answer come back without the bytes
being read.

Fixture `large-static`: 500 lines (at the budget), 72 refs, 26.6 KB of outline,
19.1 ms to load.

| operation | walker | Read IR | ratio |
|---|---|---|---|
| capture | 0.154 ms | 0.093 ms | 1.7x |
| render | 0.118 ms | 0.023 ms | 5.1x |
| capture + render | 0.275 ms | 0.115 ms | **2.4x** |
| capture, then materialise a `Snapshot` | 0.154 ms | 0.142 ms | 1.1x |
| capture, then the refs alone | 0.156 ms | 0.104 ms | 1.5x |
| delta, unchanged | 0.003 ms | 0.004 ms | 1.0x |
| delta, one line inserted | 0.661 ms | 0.757 ms | 0.9x |

Allocations for the same fixture, which is where the time went:

| operation | walker | Read IR |
|---|---|---|
| capture | 4,376 / 141 KiB | 2,024 / 114 KiB |
| render | 4,014 / 115 KiB | **3** / 34 KiB |
| capture + render | 8,390 / 256 KiB | 2,027 / 148 KiB |
| materialise a `Snapshot` | 4,376 / 141 KiB | 3,586 / **196 KiB** |
| delta, unchanged | 1 / 0 KiB | 1 / 0 KiB |

The delta row needs the phase 0 column to mean anything. Before any of this,
the same unchanged delta cost **0.999 ms, 3,454 allocations and 1,108 KiB**,
four times the cost of capturing the page it was reporting no change to.

Four results worth stating plainly, including the two that are not wins:

**The most expensive thing in a read was the unchanged delta, and it was not
the IR that fixed it.** Answering "nothing moved" built two identity vectors
and a 1.1 MB quadratic table. The fix is nine lines in `Snapshot::delta`: when
every line's identity matches, a longest common subsequence of a sequence with
itself is the whole sequence, so the answer is assembled directly. One
allocation, no table. That is a provable equivalence rather than a heuristic,
it is pinned by a differential test against the algorithm it replaced, and it
needed nothing from this design. Reported separately because folding it into
the IR's numbers would be false.

**Rendering is where the IR itself wins, and it wins by not allocating.** Three
allocations to render a 26 KB outline, against 4,014. The arena and the
`&'static str` roles mean the outline is one buffer and some slices; `format!`
per field and a fresh indent string per line were most of the old cost. On the
form-heavy fixture the same change is 10.5x.

**Materialising a `Snapshot` from the IR is not worth doing, and the number
says so.** 1.1x on time, and it allocates *more bytes than the walker* (196 KiB
against 141), because the page is then held in two shapes at once. This is the
trap the design warned about under "Expected gains", now with a measurement
attached: the IR pays only when the old shape goes away. So `Page::snapshot`
deliberately still uses the walker, and the one caller switched over is
`Page::text`, which wanted the words alone and was building a whole `Snapshot`
to discard most of it.

**Sizing the arena from the line budget made small pages worse, and the bench
caught it.** Reserving 500 lines for a 40-line page had the IR using 26.8 KiB
where the walker used 10.9. Reserving a floor of 64 and letting it grow costs a
few doublings on a large page, which the allocation count does not notice, and
the same fixture now reads at 5.5 KiB, half the walker's.

### Against the predictions

The cold-read prediction was 0 to 15%, and the honest answer is at the bottom
of that range. Loading `large-static` costs 19.1 ms and the read is 0.28 ms of
it, so a 2.4x faster read moves an `open` by about 1.2%. Parse, style and
script dominate a first read, exactly as Amdahl said.

The agent-loop prediction is the one that mattered, and it held. A
`snapshot --delta` step on an already-loaded page cost 0.43 ms of capture and
render plus 1.00 ms of delta at the phase 0 baseline; it now costs 0.115 ms
plus 0.004 ms. Allocations for that step, which are exact rather than
load-dependent, went from **11,844 to 2,028**. That is the design's actual
claim, each step costing what the step changed rather than what the page
weighs, and it is now true for the unchanged case and half true for the rest.

### What is still on the table

IR capture still makes about 2,000 allocations for 500 lines, four per line,
and nearly all of them are in the accessible-name computation: it returns an
owned `String` per named node, and `label_for` and `labelled_by` still scan the
whole document per control. The form-heavy fixture shows the cost, at 0.45 µs
per line against 0.19 µs for prose. The design's `names.rs`, an id index and a
`label[for]` reverse index built once per capture, is the next measured lever.

The changed-page delta is 0.9x, slightly worse, because the IR path
materialises both readings and hands the question to the walker's subsequence.
That is the honest shape of a transient tree, and it is what phase 3's change
log exists to fix.

## Rollout

**Phase 0, instrumentation. Landed.** Split-timing and allocation counters for
the current capture / render / delta pipeline, in
`crates/h5i-browser/benches/read.rs`. No behaviour change. Everything
after this phase is judged against these numbers, which are in
[Measured](#measured).

**Phase 1, transient IR. Landed.** `ReadRole`, the compact node, the text
arena, direct rendering from the IR, byte-compatible output. No retained cache,
no mutation hooks. The role table now returns the enum, so the walker and the
IR share one role vocabulary rather than two that could drift.

**Phase 2, stable refs and the retained cache. Next, and now justified by a
measurement.** Document epoch on `Page`, monotonic `RefId` with tombstones, the
cache held across verbs in a live session, and the fast path for unchanged
snapshot/delta. Action verbs stop taking a full capture per action. This is the
phase that lets `Session` retain a `ReadTree` instead of a `Snapshot`, which is
the precondition for the IR paying on the snapshot verb at all: while both
shapes are held, converting between them costs more than it saves.

**Phase 3, incremental invalidation.** Mutation classification at
`guard_mutation` and the native mutators, the name relation index, dirty
subtree rebuilds, the bounded change log, and the LCS deleted.

**Phase 4, shared markdown semantics.** The markdown walker moves onto the
IR; the hidden-judgment gaps close as a reviewed semantics change. Metadata
and arbitrary extract stay on the DOM.

**Phase 5, semantic execution (separate design).** Only if post-IR profiles
show style/layout/paint dominating reads: defer visual state until a
screenshot or geometry request. Touches Blitz; not part of this spec.

## Expected gains, and the adoption decision

Honest framing, because the headline benchmark barely moves:

| Operation | Expectation |
|---|---|
| cold `browser read` (network, parse, script, resolve dominate) | 0 to 15% |
| first full snapshot in a session | 1.2 to 2x on the snapshot stage |
| full snapshot, page unchanged | 2 to 5x |
| `--delta`, page unchanged | 5 to 20x+ |
| `--delta` after a small mutation | 3 to 10x |
| per-action overhead (the internal capture each verb takes) | removed |
| `wait_for` polling | one walk total instead of one per poll |

Amdahl bounds the cold read: if the snapshot stage is 20% of end-to-end and
the IR makes it five times faster, the read improves about 19%, and less if
the network or Blitz's resolve dominates, which on real pages it does.

Memory: the IR alone should cut snapshot-related memory 40 to 70%, worth
maybe 5 to 15% of engine peak RSS, and only if the old shapes actually go
away in the same change: the retained `last_snapshot: Option<Snapshot>`
clone in `Session`, the `Vec<Line>` intermediate, and the per-delta identity
vectors and LCS table. Holding both representations would raise RSS, not
lower it. The larger memory prize is Phase 5's deferred layout, which is why
it is named here and deliberately not designed here.

The decision gate for retained mode: Phase 1 numbers must show the walk and
its allocations are a meaningful share of read latency or RSS, the transient
IR must not measurably worsen cold reads, and the retained arena must cost
less than the repeated walks it replaces. If profiling instead shows parse,
Stylo, Boa, images or fonts utterly dominant, the IR narrows to what is
unconditionally right regardless of speed: stable refs, the O(1) unchanged
delta, and one shared hidden-judgment for every reading of a page. That
last sentence is the real point of the design. The win is not turning 3x
into 5x on a benchmark; it is making each agent step cost what the step
changed, and making every reader of a page read the same page.
