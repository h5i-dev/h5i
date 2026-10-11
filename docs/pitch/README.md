# h5i pitch deck

The eight-slide deck follows the docs' visual system: Archivo display type,
IBM Plex Mono labels, warm monochrome surfaces, square cells, hairline rules
and red accents. `index.html` is the content source; `deck.css` and `deck.js`
provide presentation, mobile reading, keyboard navigation and print layouts.
Without JavaScript, every slide remains readable in order.

Keep the story product-led: problem, workflow, technical depth, positioning,
then early public interest. Customer development belongs briefly at the end;
there are no customers to claim yet. Downloads and stars are interest signals,
not active-user, retention or revenue metrics.

The founding insight comes from the founder's PhD: finding bugs and proving
correctness are two sides of the same coin, and secure software needs both.
This is the thesis of the deck; AI/human collaboration explains how the
red-teaming side works. Do not substitute that workflow for the founding thesis.
Do not imply that the PhD credential establishes universal product guarantees;
proof scope and assumptions remain explicit.

Present exactly two product pillars: AI-led red-teaming and formal verification.
CLI and dashboard are the two complementary interfaces of the red-teaming
workflow, not extra product pillars. Keep that operation/oversight flow inside
the red-teaming column; it must not span both product pillars.

For h5i websec, lead with the AI-friendly CLI and the dashboard that lets a
human understand and manage the agent's work. Capture, editing and replay are
supporting tools, not the differentiator. The workflow illustration shows
both interfaces to one session. Dashboard controls refer to browser control
and stopping the browser, not arbitrary agent termination or policy editing.

The October 11, 2026 traction snapshot is maintainer-reported: **1.6k+ release
downloads and 700+ GitHub stars**. The CLI/dashboard example is synthetic
and labeled illustrative. Product boundaries follow `docs/app/TRUST.md` and
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
