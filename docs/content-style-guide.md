# h5i Content Style Guide

How every blog post and guide on h5i.dev should sound, flow, and rank. The
**pitch deck** (`/pitch/`) is the reference for *voice*. It is **not** the
reference for *structure* — guides and reference posts have different jobs.
Share the voice; pick the template that fits the page.

---

## 1. Voice — borrowed from the pitch deck

The deck's voice is the project's strongest asset. Five rules:

1. **Confident and declarative.** State the claim, then back it. No "we think",
   "arguably", "it could be argued". The deck says *"One agent is a single point
   of failure."* — not *"single agents may sometimes be less reliable."*
2. **Concrete over abstract.** Name the command, the ref, the number. *"capture
   cut tool-output tokens by ~95%"* beats *"capture significantly reduces token
   usage."*
3. **Contrast-driven.** Define by opposition. The signature move:
   > Git records **what** changed. h5i records **the rest** — the prompt, the
   > model, the reasoning, the verdict.
4. **One idea per sentence, short sentences carry the punch.** Let a long
   explanatory sentence be followed by a 4-word verdict.
5. **No marketing fog.** Avoid "revolutionary", "seamless", "powerful",
   "cutting-edge", "game-changing". The product is opinionated; the prose should
   be plain.

### Sentence patterns to reuse
- **Colon reveal:** `The winning unit is not the best single agent: it is the best managed agent team.`
- **Not-X-but-Y:** `Not a group chat. Not a daemon. A thin coordination layer.`
- **The triad:** `isolate the work, summarize the evidence, keep raw noise out of context.`
- **Stakes line:** a one-sentence "why this bites" after the setup.

---

## 2. Canonical vocabulary

Use these exact terms; they are how the project talks about itself, so reusing
them compounds brand + keyword consistency.

| Use | Not |
|---|---|
| agent **ensemble** / agent **team** | "swarm", "fleet", "crowd" |
| **independent / sealed** attempts | "parallel runs" |
| **neutral verifier** | "judge AI", "evaluator model" |
| **auditable workspace** | "logged session" |
| **Git-native** / **Git sidecar** | "plugin", "wrapper", "SaaS" |
| **provenance** (prompt · model · agent) | "metadata" (when you mean provenance) |
| **capture** (compact tool output) | "logging" |
| **i5h messaging** / `refs/h5i/msg` | "chat", "inbox" (except UI labels) |
| **sandboxed env** / **confined** | "VM", "container" (unless literally that tier) |
| **no SaaS, no lock-in**, lives in `refs/h5i/*` | "our platform", "our cloud" |

Always: `h5i` lowercase, pronounced *high-five*. Commands in `<code>`.

---

## 3. The hook formula (every page, first 2–3 sentences)

> **Tension → Stakes → Turn**

1. **Tension:** what Git / a single agent / the naive setup *cannot* do.
2. **Stakes:** the one sentence on why that bites in practice.
3. **Turn:** what h5i gives you instead — concrete, with the command or ref.

This is the one pitch move that travels to *every* content type. Most current
posts open with neutral exposition; lead with the hook instead.

---

## 4. Templates — pick by the page's job

Share the voice; do **not** force the deck's 13-beat arc onto a how-to.

### A. Explainer / pillar post (`what is…`, `why…`)
Reader arrived from search wanting an answer. Be answer-first.
1. **Eyebrow** + H1 (keyword-forward).
2. **Answer-first TL;DR** — a `.callout` box that defines the term in 1–2
   sentences. (LLM answer engines and featured snippets quote this.)
3. **Hook** (§3).
4. Body H2s, several phrased **as questions** ("Why isn't Git enough?").
5. **FAQ** with `FAQPage` schema (§5).
6. **Sources and verification.**
7. Next-up link + CTA.

### B. Comparison / opinion (`X vs h5i`, `why diffs aren't enough`)
1. Eyebrow + H1.
2. **One-line verdict** in a `.callout` (the answer-engine pull-quote).
3. Hook framing the tension as a real decision.
4. **Comparison table** (`.tbl-wrap`) — scannable, the asset answer engines lift.
5. "When X is still the right call" (earns trust by conceding).
6. FAQ schema + Sources + CTA.

### C. How-to guide (`/guides/*`)
Reader wants the command now. **Keep it imperative and numbered — do not
narrate.** Voice shows up only in the intro and the callouts.
1. Eyebrow + H1 (task-shaped: "Keep tool output out of your agent's context").
2. **2-sentence hook** — the problem and the one-line fix.
3. **Numbered steps**, each an imperative verb ("Wrap a command").
4. **Copy-paste blocks** that actually run.
5. A `.callout` for the one gotcha.
6. FAQ schema (optional) + "Related guides".

---

## 5. SEO + AEO checklist (apply to every page)

**Classic SEO**
- [ ] One `<h1>`, keyword in the first 100 words and in the H1.
- [ ] `<title>` ≤ ~60 chars, primary keyword first, **no duplicated site
      suffix** (use a single ` | h5i` or none — never `…: h5i: h5i`).
- [ ] `<meta name="description">` 140–160 chars, leads with the answer.
- [ ] `rel="canonical"`, OG + Twitter card, descriptive `alt` on every image.
- [ ] Internal links to 2–3 sibling posts/guides with descriptive anchors.
- [ ] `dateModified` bumped whenever content changes substantively.

**AEO (answer-engine / AI-search optimization)**
- [ ] **Answer-first:** the opening `.callout` answers the title question in 1–2
      self-contained sentences (no "as described above"). This is the chunk an
      LLM lifts.
- [ ] **Question headings:** phrase H2/H3 as the questions people actually ask.
- [ ] **`FAQPage` JSON-LD** mirroring the visible FAQ, verbatim Q&A.
- [ ] **`TechArticle`/`Article` JSON-LD** with `headline`, `description`,
      `author`, `datePublished`, `dateModified`, `keywords`, `mainEntityOfPage`.
- [ ] **`BreadcrumbList` JSON-LD** (Home › Blog/Guides › This page).
- [ ] **Self-contained claims:** each section stands alone when quoted out of
      context — spell out "h5i" and the key noun rather than relying on "it".
- [ ] **Definitions and numbers near the top**, stated as plain facts an engine
      can extract ("h5i stores this in `refs/h5i/*`").

---

## 6. Quick before/after

> **Before (flat):** "Git records what changed. It does not record why, by whom,
> with what context, or whether the result was tested."
>
> **After (pitch voice):** "Git records **what** changed — and stops there. It
> can't tell you *who* wrote a line (agent or human), *what* prompt produced it,
> or *whether* it was tested. h5i records that rest, in `refs/h5i/*`, beside your
> code."

Same facts. Confident, concrete, contrast-driven, keyword-dense, and the first
two sentences are quotable on their own.
