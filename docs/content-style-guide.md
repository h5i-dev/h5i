# h5i editorial guide

The documentation has four jobs. Product pages answer what h5i is. Guides help
a reader finish a task. The manual defines commands and fields. Blog essays
explain durable design choices. Do not make one page perform another layer's
job.

## Keep the collection small

A new page needs a job no existing page can do.

- Extend a guide when the reader is still pursuing the same outcome.
- Extend the manual when the material defines a command, field, or limit.
- Extend an essay when the material supports the same central claim.
- Add a page only for a genuinely different reader, outcome, or argument.

Never split one subject into a series to manufacture volume. Redirect retired
URLs to the closest replacement. Keep redirects out of indexes, feeds, the
sitemap, and llms.txt.

## Voice

h5i is confident, concrete, and honest about boundaries.

1. State the claim early.
2. Name the command, mechanism, or limitation that supports it.
3. Prefer short sentences at the moment the argument turns.
4. Use contrast when it clarifies a boundary: state versus execution,
   testimony versus observation, portability versus network enforcement.
5. Avoid marketing fog such as seamless, powerful, revolutionary, and
   game-changing.

Use h5i in lowercase. A disposable environment is a box. The shared,
Git-backed conversation between boxes is the forum, and the command that
operates it is `h5i forum`. The security property is a boundary or confinement.
Use receipt for the execution record and output gate for the human-operated
export step. A post's identity is host-stamped; a thread's policy limit is a
ceiling; participants hold a role.

Say host-observed for what this machine recorded and peer-claimed for what
arrived from a machine it cannot verify. Never merge the two into one label.
The one-line statement of the product's central property is "agents share
information, never permissions"; use it where it does work, not as a refrain.

Do not resurrect removed product language. h5i is not a provenance system, an
agent ensemble, an orchestra, or an AI-aware version-control layer. Do not call
the forum a board, a bus, a channel, or a chat: those names each imply a wire
between agents, and there is none.

## Guides

A guide is imperative and outcome-shaped. It contains:

1. A short explanation of why the task needs a box.
2. An outcome callout.
3. Numbered steps with imperative headings.
4. Commands that match the current manual.
5. A check after every consequential action.
6. The security gotcha most likely to change the decision.
7. A stopping point: export, apply, or remove.
8. Links to the relevant manual section and the next guide.

Do not narrate product history in a guide. Do not hide prerequisites in the
third step. Do not show fictional output as if it came from a real run.

## Blog essays

An essay earns its place by making one durable argument:

1. Claim: one self-contained answer in the opening callout.
2. Tension: the familiar approach and the limit it reaches.
3. Mechanism: the concrete design choice that changes the result.
4. Tradeoff: what the design does not solve or makes worse.
5. Practical test: questions the reader can apply elsewhere.

The blog is not a changelog, vulnerability feed, benchmark archive, or release
announcement surface.

## Editorial depth

Published essays should normally reach 1,800–2,800 words. Guides should usually
reach 1,000–1,500 words without delaying the first runnable command. Word count
is a floor for developed reasoning, not a target to pad.

Every canonical page needs at least one useful visual: an architecture diagram,
evidence screenshot, decision table, or workflow figure. The visual must teach
a relationship the prose would otherwise make the reader reconstruct.

An essay should include a concrete failure or run, implementation-level
mechanism, the tradeoff that mechanism introduces, and sources. A guide should
include expected evidence, common failure modes, and a clear stopping point.

## Claims and limits

Name the layer and the observer.

- supervised and microvm enforce egress at L3/L4.
- container uses an L7 proxy allowlist.
- Every tier below microvm shares the host kernel.
- A message carries no capability, and h5i does not classify message content.
- A local post is host-observed. A remote one is peer-claimed and unverified.
- `forum attach` refuses a workspace-tier box; that tier enforces nothing.
- A host-observed exit is evidence. An agent-authored summary is testimony.
- A receipt is protected from the box, not notarized against the host owner.
- Containment does not stop source from entering an allowed model request.

If a section is unavailable, say why. Absence must not impersonate success.

## Page mechanics

Every canonical article needs one H1 and a contiguous heading outline;
descriptive metadata; canonical, Open Graph, and Twitter tags, including an
image alt; TechArticle and BreadcrumbList JSON-LD; visible FAQ text when
FAQPage data is present; useful internal links; a current dateModified; and
inclusion in sitemap.xml. Blog essays also enter feed.xml.

A title should fit in about 60 characters and a meta description in about 160,
because that is where a search result cuts them. When a page's card blurb is
worth more room than that, give it a shorter `meta` line as well.

Dates are not decorative. `PAGE_HISTORY` in this file records each page's last
content change beside a fingerprint of what it contained, and the build refuses
to finish when a fingerprint moves without its date, so `lastmod` and
`dateModified` cannot quietly describe a version that no longer ships.

One entity, one @id. The product is `https://h5i.dev/#app` and the site is
`#website` on every page that names them; a page-scoped node (`#faq`,
`#breadcrumb`, `#webpage`) is scoped to that page's URL. Two pages describing
one @id differently, or one page carrying two BreadcrumbLists, leaves a crawler
picking between them. A breadcrumb is the trail to the page it sits on, so the
home page has none.

Before publishing, remove repeated setup, claims without mechanisms, invented
precision, and references to features the manual no longer documents. Then
read the opening callout and every heading without the body. They should still
tell the whole story.
