# h5i pitch deck

The eight-slide deck follows the docs' visual system: Archivo display type,
IBM Plex Mono labels, warm monochrome surfaces, square cells, hairline rules
and red accents. `index.html` is the content source; `deck.css` and `deck.js`
provide presentation, mobile reading, keyboard navigation and print layouts.
Without JavaScript, every slide remains readable in order.

Keep the opening question and the founding insight about finding bugs and
proving properties. Present h5i as a Rust web framework and CLI, then show
Build → Verify → Hack with the existing code and UI evidence. Follow with
early public interest. Customer development belongs briefly at the end;
there are no customers to claim yet. Downloads and stars are interest signals,
not active-user, retention or revenue metrics.

The founding insight comes from the founder's PhD: finding bugs and proving
correctness are two sides of the same coin, and secure software needs both.
This is the thesis of the deck; AI/human collaboration explains how the
red-teaming side works. Do not substitute that workflow for the founding thesis.
Do not imply that the PhD credential establishes universal product guarantees;
proof scope and assumptions remain explicit.

The product overview shows build with composable Rust kernels, verify stated
properties in Lean 4, and red-team the running app in a two-by-two layout. The
contrasting fourth cell shows how h5i connects the work through its CLI,
dashboard, and sandbox. Browser, traffic capture, recon, and CI/CD tests explain
the red-teaming step. Keep GPU as a supporting kernel capability rather than
making it a separate product claim.

Slide 4 explains the build architecture as Axum → pure Rust kernel → storage.
Keep it to one diagram; slide 5 carries the Rust and Lean code examples.

For h5i websec, lead with the AI-friendly CLI and the dashboard that lets a
human understand and manage the agent's work. Capture, editing and replay are
supporting tools, not the differentiator. Show the real dashboard layout, not a simplified mockup. Use native HTML/CSS
so its text and borders remain sharp in the PDF. Dashboard controls refer to browser control
and stopping the browser, not arbitrary agent termination or policy editing.

The October 18, 2026 GitHub API snapshot reports **788 stars and 4,213 total
release asset downloads** across 41 releases. The deck uses conservative
floors of 780+ and 4.2k+. These are public interest signals, not customer or
revenue metrics. The dashboard uses a static DOM/CSS snapshot of the real `h5i ui`, captured
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
paragraphs. Slide 4 shows the actual Rust delete function from the board tutorial,
with the web, kernel, and storage boundary beside it. Slide 5 repeats the
relevant authorization branch and shows the Lean `authorized` theorem statement.
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
