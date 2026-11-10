# h5i pitch deck

The six-slide deck follows the docs' visual system: Archivo display type,
IBM Plex Mono labels, warm monochrome surfaces, square cells, hairline rules
and red accents. `index.html` is the content source; `deck.css` and `deck.js`
provide presentation, mobile reading, keyboard navigation and print layouts.
Without JavaScript, every slide remains readable in order.

The deck tells one story: h5i is a development platform for proving that
the critical properties of web software are correct. Six slides: product,
problem, solution, traction, business model, founder and ask. Do not frame
formal verification as bug finding; it guarantees properties. The solution
slide shows the framework's architecture, not a translation step: HTTP /
Axum, then the Rust app kernel, then the database, with the Lean 4 proof
under the kernel. A Rust-to-Lean picture reads as Aeneas. Rust-to-Lean translation itself is Aeneas,
not h5i's invention, so never claim it as unique. Red-teaming, market sizing,
architecture, property counts and vulnerability counts stay out. The YC guide
is not a checklist. One idea per slide, **20 to 30 words at most** (title and
body; eyebrow and folio excluded). Say "formally verify", not
"mathematically verify". No clipped "A. B." slogans and no noun phrases joined by a comma. The landing page does not link here on purpose.

The cover shows stars, forks (80) and downloads in a right-hand column.
The traction slide embeds the live star-history.com chart (dark theme), so
the PDF render needs network access to include it. Beside it: 80 forks and
20+ contributors (23 on GitHub, November 10, 2026). Do not say "active":
only about ten committed in the last 90 days. The OS donut uses the same
download run: Linux 2,323, macOS 885, Windows 689 of 3,897.
Traction: 913 stars and 3,897 release downloads (Python wheels excluded, via
`scripts/h5i-stat.sh`), both read from GitHub on November 9, 2026. The business model slide sells verification infrastructure, not a
dashboard: free is the web framework, Lean 4 proofs and local CLI; paid
(planned) is continuous verification, proof monitoring in CI/CD, and team
collaboration with audit reports. It stacks the free and paid cards on the left and
embeds `assets/h5i-ui-session.html` (the static `h5i ui` capture) on the
right. The paid
tier is labeled "planned" because no paid product exists. The ask says
"seeking design partners and seed investors"; say "raising" only once a round
is formally open. Publications come from koukyosyumei.github.io. The "100+ zero-day
vulnerabilities and CVEs" figure is the founder's own count; the homepage
lists only five selected findings, so keep a source for it on hand.

Neither slides nor speaker notes should imply universal product guarantees:
proofs establish stated properties for all modeled inputs under explicit
assumptions, and product boundaries follow `docs/design/design-app-trust.md`.
Do not fill gaps with invented customer numbers, pricing or market statistics.

The session capture on slide 5 used an isolated `H5I_BROWSER_HOME` and
`H5I_PROJECT_HOME`, plus a local HTTP demo with orders 1041/Alice and
1042/Bob. Actual browser navigation, a replay changing the ID, and a finding
populated the UI. It represents a demonstration, not customer evidence.
Runtime scripts and the UI access token are excluded from the artifact; the
iframe is sandboxed and static. `assets/h5i-app-ui.html` (the board Apps
guarantee graph) and `assets/rust-lean-proof.svg` are kept for reuse but no
slide embeds them now.

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

Keep slides readable at a glance: short labels, cards and one code example at
most. Content-heavy slides (4 and 5) carry the `top` class and start near the
top; the others center vertically. Check every slide at 1280 × 720 for
overflow past the folio, at phone widths, and in the PDF.
