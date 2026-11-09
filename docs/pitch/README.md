# h5i pitch deck

The ten-slide deck follows the docs' visual system: Archivo display type,
IBM Plex Mono labels, warm monochrome surfaces, square cells, hairline rules
and red accents. `index.html` is the content source; `deck.css` and `deck.js`
provide presentation, mobile reading, keyboard navigation and print layouts.
Without JavaScript, every slide remains readable in order.

The deck is written for investors, in the seed-deck order YC recommends:
what h5i is, problem, customers, solution, insight, traction, market,
business model, founder, the ask. One idea per slide, **20 to 30 words at
most** (title and body; eyebrow and folio excluded). Detail belongs in the
speaker's mouth and the docs, not on the slide. Say "formally verify", not
"mathematically verify". The landing page does not link here on purpose.

Traction shows only GitHub stars and release downloads (both read from
GitHub on November 9, 2026: 913 stars; 3,897 downloads via
`scripts/h5i-stat.sh`, Python wheels excluded). They are interest signals,
not active-user, retention or revenue metrics. The paid tier on the business
model slide is labeled "planned" because no paid product exists yet. The
founder's publications come from koukyosyumei.github.io. Slide 5 embeds
`assets/h5i-ui-session.html`, a static DOM/CSS capture of the real `h5i ui`
from a local demo (orders 1041/Alice and 1042/Bob), not customer evidence.

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
