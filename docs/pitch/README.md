# h5i pitch deck

The eight-slide deck follows the docs' visual system: Archivo display type,
IBM Plex Mono labels, warm monochrome surfaces, square cells, hairline rules
and red accents. `index.html` is the content source; `deck.css` and `deck.js`
provide presentation, mobile reading, keyboard navigation and print layouts.
Without JavaScript, every slide remains readable in order.

Keep the opening question, then explain why finding bugs cannot establish that
critical properties always hold. Present h5i as a verifiable Rust web framework, supported
by development infrastructure that also works with existing web apps. Slides
4 and 5 distinguish the kernel architecture from the properties proved with
it; slide 6 demonstrates red-teaming tools. Customer development belongs
briefly at the end; there are no customers to claim yet. Downloads and stars
are interest signals, not active-user, retention or revenue metrics.

Slide 2 makes the case for formal verification: testing and red-teaming can
uncover vulnerabilities, while proofs establish stated properties for all
modeled inputs under explicit assumptions. Neither slide nor speaker notes
should imply universal product guarantees. Slide 6 shows how red-teaming
complements proofs by testing HTTP adapters, external services and other
behavior outside the proof boundary.

Slide 3 has two levels: the framework is the core product, and the CLI,
dashboard and sandbox are development infrastructure. Kernel extraction to
Lean 4 is the main architecture point. Keep GPU as an optional supporting
kernel capability. Slide 4 shows Rust branching to Lean 4 verification and
optional Rust-CUDA execution; Lean does not generate the GPU code or prove
CPU/GPU equivalence.

Slide 5 keeps the Rust authorization branch, Lean theorem and actual h5i UI
guarantee graph. Slide 6 keeps the shell and dashboard demonstration, with an
explicit note that the tools work on applications outside h5i-app.

For h5i websec, lead with the AI-friendly CLI and the dashboard that lets a
human understand and manage the agent's work. Capture, editing and replay are
supporting tools, not the differentiator. Show the real dashboard layout, not a simplified mockup. Use native HTML/CSS
so its text and borders remain sharp in the PDF. Dashboard controls refer to browser control
and stopping the browser, not arbitrary agent termination or policy editing.

Slide 7 shows an October 21, 2026 snapshot: 2.1k+ release downloads and
790+ GitHub stars. These are conservative interest signals, not
customer numbers. The dashboard uses a static DOM/CSS snapshot of the real `h5i ui`, captured
from a local demonstration session. It is not a screenshot or a UI mockup. Product boundaries follow `docs/app/TRUST.md` and
the box policy guide. Design partners and paid pilots are future steps.

For an investor-specific version, add founder credentials, the funding ask
and use of funds when available. Add commercial validation and a defensible
market estimate when evidence supports them; do not fill gaps with invented
customer numbers, pricing or market statistics.

## Refresh the PDF

From the repository root:

```sh
node docs/pitch/render-pdf.mjs
```

Commit `h5i-pitch-deck.pdf` alongside source edits. The renderer serves `docs/`
locally and uses the deck's print rules for one 1280 × 720 page per slide.
It requires Playwright and a Chromium installation (see the renderer).
Google Fonts supplies the same fonts as the docs; network access is needed
for the intended typography. Wait for `document.fonts.ready` before captures.

When the slide count changes, update the PDF viewer's metadata and accessible
label in `pdf/index.html`. Update the `pitch/` content fingerprint in
`docs/build-content.py`, then run the docs build. Verify all slides at desktop
size, phone widths, keyboard/deep-link navigation and print output.

## Visuals

Keep slides readable at a glance. Use short labels and diagrams instead of
paragraphs. Slide 4 shows the Rust `push_rule` kernel from the parallel example,
with branches to its Lean proof and optional CUDA wrapper. Slide 5 shows the
board tutorial's delete authorization branch and Lean `authorized` theorem statement.
The proof body is explicitly
omitted. Its Apps panel is a static capture of the actual `.app-map` guarantee graph
component. No successful check or proof-run status has been fabricated.
Slide 6 embeds `assets/h5i-ui-session.html`: the actual UI DOM and CSS captured from
`target/release/h5i ui` (binary built October 8, capture October 11, 2026).

The capture used an isolated `H5I_BROWSER_HOME` and `H5I_PROJECT_HOME`, plus a
local HTTP demo with orders 1041/Alice and 1042/Bob. Actual browser navigation,
a replay changing the ID, and a finding populated the UI. It represents a
demonstration, not customer evidence. Runtime scripts and the UI access token
are excluded from the artifact; the iframe is sandboxed and static.

Slide 6 pairs an illustrative shell transcript with the actual session
workspace (`assets/h5i-ui-session.html`). The same captured session populates
the history. Slide 5 uses `assets/h5i-app-ui.html` from the real board Apps view.

The Apps graph panel focuses the captured SVG on its guarantee, theorem and
specification columns (viewBox 0 0 780 270), labeled as an excerpt. The node
labels and connections come from the actual UI; they are not invented proof
check results.
