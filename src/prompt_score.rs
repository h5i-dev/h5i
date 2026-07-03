//! Offline **Prompt Maturity Score** — a classical-NLP measure of how well an
//! engineer prompts an AI coding agent.
//!
//! # Why
//!
//! h5i already records the *prompt* that triggered each AI commit
//! (`AiMetadata::prompt`). A manager reviewing a PR can see *what* was asked,
//! but has no objective read on *how well* it was asked. This module turns the
//! prompts on a branch into a single, explainable 0–100 score so that prompt
//! craft becomes a visible, trackable signal — surfaced in the
//! `h5i share pr post` body.
//!
//! # Constraints (design contract)
//!
//! * **Fully offline, deterministic.** Readability indices, lexical-diversity
//!   measures, and curated lexicons only — no LLM, no network. Reproducible in
//!   CI and Git hooks.
//! * **Bilingual (English + Japanese), dictionary-free.** The scorer is
//!   script-aware ([`Lang`]/[`detect_lang`]). Japanese is scored on a dedicated
//!   path — segmented by character-class run ([`tokenize_ja`]) and matched
//!   against parallel `JA_*` lexicons by substring — with **no morphological
//!   analyser / MeCab dependency**, to keep the offline contract. The slot
//!   rubric, multiplicative core, and aggregation are language-neutral (they
//!   consume feature *counts*). Readability indices are English-specific, so
//!   Japanese uses a sentence-length clarity proxy. Other languages **abstain**
//!   (`unscored`) rather than being mis-scored. English calibration is
//!   unaffected. See the "Language detection & Japanese support" section.
//! * **Readability ≠ maturity.** A terse, precise ask
//!   (*"fix the off-by-one in `parse_range()` in src/util.rs, add a test"*) is
//!   an *excellent* prompt that scores badly on raw reading-ease. So readability
//!   is used only as a **trapezoid band** (both extremes penalised), is one of
//!   the lowest-weighted signals, and is computed on a **code-masked** copy of
//!   the text so file paths and `func()` tokens don't corrupt the syllable
//!   counter.
//!
//! # The composite (v2 — slot rubric + multiplicative core)
//!
//! Every signal is normalised to `0.0..=1.0`. Three **core slots** are
//! *necessary* and combine **multiplicatively** (each floored at [`CORE_MIN`]);
//! the rest are **enrichment** that lifts an already-solid core additively (see
//! [`ENRICHMENT`], which sums to 1.0). See the "Aggregation model (v2)" section
//! below for the full formula and the rationale for replacing v1's flat
//! weighted sum + balance-gate caps.
//!
//! | Slot          | Role        | Captures                                        |
//! |---------------|-------------|-------------------------------------------------|
//! | `objective`   | core (×)    | a positive, actionable goal − vague words       |
//! | `grounding`   | core (×)    | concrete refs: paths, `func()`, idents, numbers |
//! | `direction`   | core (×)    | acceptance · constraints · pre/post/IO/exception|
//! | `context`     | enrichment  | background, goal/why, current state             |
//! | `examples`    | enrichment  | examples, doctests, input→output illustrations  |
//! | `structure`   | enrichment  | decomposition — bullets, steps, headings        |
//! | `diversity`   | enrichment  | lexical richness (adaptive MATTR)               |
//! | `clarity`     | enrichment  | readability in a target band (trapezoid)        |
//! | `adequacy`    | enrichment  | authored length in a sweet spot                 |
//!
//! On top sit **anti-gaming guards**: per-category keyword caps, a repetition
//! penalty, a vagueness penalty (folded into the core), a single short-prompt
//! floor, and a saturating **evidence** bonus for attached machine output. The
//! multiplicative core means a prompt empty on any necessary axis is *capped*
//! with no special-case gate — a keyword-stuffed but ungrounded prompt can never
//! read as "advanced". Prompts the heuristics can't assess (no authored request,
//! non-English) are **abstained** on rather than mis-scored.
//!
//! # Artifact segmentation (v2) — craft ≠ paste volume
//!
//! Real prompts routinely *contain machine output*: error logs, stack traces,
//! compiler diagnostics, test-runner output, diffs. That text was **pasted, not
//! written** — yet to v1 it looked like craft: paths and numbers farmed
//! `specificity`, `test`/`line`/`file` tokens farmed verification and
//! grounding, and sheer length lifted the adequacy curve and hard length caps.
//! Measured on a real cargo-test failure paste, `"fix this failing test"` went
//! 18 → 32 just by attaching the log — while a *well-crafted* prompt **dropped**
//! 53 → 35 when the same log was attached (the paste drowned its diversity and
//! tripped the repetition penalty). Wrong in both directions.
//!
//! v2 therefore splits the prompt line-wise into **authored prose** and
//! **pasted artifact** ([`segment_artifacts`]: stack-frame / diagnostic /
//! test-runner / log-level / timestamp / diff-marker patterns seed blocks;
//! machine-ish neighbours — stopword-free, symbol-dense, deeply indented, or
//! code-token-majority lines — join by contagion; fenced ``` blocks are
//! artifact by construction). Every craft signal, the length caps, and the
//! branch length-weighting are computed on the **authored text only**. The
//! artifact instead feeds one new, deliberately *saturating* signal:
//! `evidence` — attaching machine output is good grounding practice, so it
//! earns a small fixed bonus (up to [`EVIDENCE_BONUS_MAX`] points, more when
//! the prose explicitly frames the paste), but it can never scale with paste
//! volume and never dilutes the authored signals.
//!
//! The segmentation features follow the natural-language-vs-machine-text
//! literature: NLoN (Mäntylä et al., MSR 2018, arXiv 1803.07292) reaches
//! AUC ≈ 0.97 line-level from exactly these cheap signals (stopword density,
//! special-character ratio, digit ratio, indentation); infoZilla and the
//! bug-report de-noising line (arXiv 2110.01336) show stack traces / diffs /
//! logs are regex-friendly with precision > 0.9. The evidence-not-length move
//! mirrors the standard length-bias countermeasure in text-quality metrics
//! (Length-Controlled AlpacaEval, arXiv 2404.04475): credit *per unit of
//! authored signal*, never total volume.
//!
//! # Scope vs. prompt-eval frameworks
//!
//! This is deliberately *not* an LLM-eval. Frameworks like PromptBench
//! (arXiv 2312.07910), APE (arXiv 2211.01910), and Promptfoo score a prompt by
//! running a model and judging its **output** — they need API access and a task
//! dataset. h5i Prompt maturity scores the **input** — the craft of the ask
//! itself — fully offline, from text features alone, so it can run in a Git hook
//! or PR render with no model call. The two are complementary: one asks "did the
//! model do well?", this asks "did the engineer ask well?". The classical
//! signals (readability indices, MATTR) are well-studied; the load-bearing
//! caveat from that literature — readability/diversity are length-sensitive and
//! punish terse technical text — is exactly why they are bounded sub-signals
//! here, never the score.

use std::collections::{HashMap, HashSet};

// ── Aggregation model (v2) ─────────────────────────────────────────────────────
//
// v1 was a flat weighted sum of seven sub-signals plus a stack of special-case
// caps (the 69/79 "balance gates", the tactical exemption, tiered length caps).
// v2 replaces both with a **slot rubric + multiplicative core** — the shape the
// bug-report-quality (CTQRS) and code-gen prompt-guideline literature
// (Midolo et al., arXiv 2601.13118) both converge on:
//
//   * Three **core slots** are *necessary* and combine **multiplicatively**, so
//     a prompt cannot look mature on one axis while empty on another (this is
//     what the balance gates hand-coded). Each slot is floored at [`CORE_MIN`]
//     so a genuine weakness *caps* the score gracefully instead of zeroing it:
//       - `objective`  — is there a positive, actionable goal? (not just "don't…")
//       - `grounding`  — concrete references: paths, `func()`, idents, numbers
//       - `direction`  — did they bound the agent: acceptance criteria,
//                        constraints, and the pre/post/IO/exception contract
//   * Five **enrichment signals** (context, examples, structure, diversity,
//     clarity, adequacy) form an *additive lift* above a floor of
//     [`ENRICHMENT_BASE`]: they can only raise a prompt that already has a solid
//     core, never rescue one that doesn't.
//   * A **repetition** multiplier and a **vagueness** penalty (folded into
//     `objective`) defeat keyword-farming; an **evidence** bonus (outside the
//     composite, saturating) credits attached machine output — see the
//     artifact-segmentation docs above.
//
// The tactical exemption falls out for free: `context` is enrichment, not core,
// so a concrete + bounded ask with no "why" keeps its core and simply forgoes
// the context lift — no special case needed.
//
// TODO(empirical-calibration, v2): the slot weights, [`CORE_MIN`], and
// [`ENRICHMENT_BASE`] are still *normative* (hand-tuned; the enrichment/direction
// weights seeded from the guideline-prevalence rates in arXiv 2601.13118 — I/O
// format 44%, post-conditions 23%, requirements 19%, …). They are an explainable
// proxy, not a validated model. `PromptScoreBreakdown` exposes every slot per
// prompt, so a future PR can join them against h5i's per-commit outcome signals
// (test pass/fail, review-flag score, churn, later reverts) and *learn* the
// weights while keeping the features explainable. Do NOT regress from a tiny
// biased sample — that needs a corpus and confound controls.

/// Per-factor floor for the three core slots. A core slot at 0 still contributes
/// `CORE_MIN` to the geometric-mean core, so a single missing axis caps the
/// score rather than annihilating it (an all-empty core equals `CORE_MIN`).
pub const CORE_MIN: f64 = 0.18;

/// Floor of the enrichment lift: a prompt with a perfect core but zero
/// enrichment still keeps this fraction of its core-driven score. Enrichment
/// fills the remaining `1 - ENRICHMENT_BASE`.
pub const ENRICHMENT_BASE: f64 = 0.63;

/// Prompts under this many *authored* words can't fully specify a coding task;
/// their score is capped at [`SHORT_PROMPT_CAP`]. Replaces v1's tiered word caps
/// with a single gentle floor so crisp 8–15 word tactical asks can still breathe.
const SHORT_PROMPT_WORDS: usize = 6;
const SHORT_PROMPT_CAP: f64 = 35.0;

/// Branch-roll-up shrinkage: the neutral prior a small sample is pulled toward,
/// and its strength in prompt-equivalents. At `n` scored prompts the branch
/// score is `(STRENGTH·PRIOR + n·mean) / (STRENGTH + n)` — so 1 prompt is pulled
/// strongly toward the prior, ~10 prompts barely at all.
pub const BRANCH_PRIOR_MEAN: f64 = 50.0;
pub const BRANCH_PRIOR_STRENGTH: f64 = 2.5;

/// Maximum bonus points the `evidence` signal can add to the composite.
/// Attaching machine output (a log, trace, diff, or fenced block) is good
/// grounding practice, so it earns a small fixed credit — deliberately a
/// *bonus outside the composite* so its absence never penalises the many prompts
/// that have no artifact to attach, and deliberately saturating so it can never
/// scale with paste volume.
pub const EVIDENCE_BONUS_MAX: f64 = 5.0;

/// Relative weights of the six enrichment signals (the additive lift above the
/// multiplicative core). Sums to 1.0 (asserted in tests). Seeded from the
/// guideline-prevalence rates in arXiv 2601.13118 where they map (I/O format and
/// examples are the most impactful enrichers observed there).
pub const ENRICHMENT: EnrichmentWeights = EnrichmentWeights {
    context: 0.30,
    examples: 0.20,
    structure: 0.20,
    diversity: 0.12,
    clarity: 0.10,
    adequacy: 0.08,
};

/// Weighting of the six enrichment signals. See [`ENRICHMENT`].
#[derive(Debug, Clone, Copy)]
pub struct EnrichmentWeights {
    pub context: f64,
    pub examples: f64,
    pub structure: f64,
    pub diversity: f64,
    pub clarity: f64,
    pub adequacy: f64,
}

impl EnrichmentWeights {
    /// The additive enrichment lift in `0.0..=1.0` for one breakdown.
    fn lift(&self, b: &PromptScoreBreakdown) -> f64 {
        (self.context * b.context
            + self.examples * b.examples
            + self.structure * b.structure
            + self.diversity * b.diversity
            + self.clarity * b.clarity
            + self.adequacy * b.adequacy)
            .clamp(0.0, 1.0)
    }

    #[cfg(test)]
    fn sum(&self) -> f64 {
        self.context + self.examples + self.structure + self.diversity + self.clarity + self.adequacy
    }
}

// ── Public result types ──────────────────────────────────────────────────────

/// The normalised sub-signals (`0.0..=1.0`) plus the raw readability numbers,
/// retained for transparent display. The first three are the multiplicative
/// **core slots**; the next six are the additive **enrichment** signals;
/// `evidence` is a saturating bonus outside the composite.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PromptScoreBreakdown {
    /// Core: a positive, actionable goal is stated (not merely prohibitions).
    pub objective: f64,
    /// Core: concrete references — paths, `func()`, identifiers, numbers.
    pub grounding: f64,
    /// Core: the agent is bounded — acceptance criteria, constraints, and the
    /// pre/post-condition · I/O-format · exception contract.
    pub direction: f64,
    /// Enrichment: background / why / current state.
    pub context: f64,
    /// Enrichment: examples, doctests, input→output illustrations.
    pub examples: f64,
    /// Enrichment: decomposition — bullets, numbered steps, headings.
    pub structure: f64,
    /// Enrichment: lexical richness (adaptive MATTR).
    pub diversity: f64,
    /// Enrichment: readability in a target band (trapezoid).
    pub clarity: f64,
    /// Enrichment: authored length in a sweet spot.
    pub adequacy: f64,
    /// Evidence signal (`0.0..=1.0`): pasted machine output / fenced blocks
    /// attached (0.7) and explicitly framed by the authored prose (up to 1.0).
    /// A *bonus* signal — not part of the composite; it adds up to
    /// [`EVIDENCE_BONUS_MAX`] points on top and its absence costs nothing.
    pub evidence: f64,
    /// Raw Flesch Reading Ease (≈0–100, higher = easier). Display-only.
    pub flesch_reading_ease: f64,
    /// Raw Flesch-Kincaid Grade Level (US school grade). Display-only.
    pub fk_grade: f64,
    /// Raw Gunning Fog index. Display-only.
    pub gunning_fog: f64,
}

impl PromptScoreBreakdown {
    fn zero() -> Self {
        PromptScoreBreakdown {
            objective: 0.0,
            grounding: 0.0,
            direction: 0.0,
            context: 0.0,
            examples: 0.0,
            structure: 0.0,
            diversity: 0.0,
            clarity: 0.0,
            adequacy: 0.0,
            evidence: 0.0,
            flesch_reading_ease: 0.0,
            fk_grade: 0.0,
            gunning_fog: 0.0,
        }
    }

    /// The multiplicative core in `[CORE_MIN, 1]`: the **geometric mean** of the
    /// three core slots, each floored at [`CORE_MIN`]. Geometric (not raw
    /// product) so three healthy-but-imperfect slots don't over-compound to a
    /// tiny number, while a single weak slot still drags the whole core down and
    /// a missing one floors it at `CORE_MIN`.
    fn core(&self) -> f64 {
        let floor = |x: f64| CORE_MIN + (1.0 - CORE_MIN) * x.clamp(0.0, 1.0);
        (floor(self.objective) * floor(self.grounding) * floor(self.direction)).cbrt()
    }
}

/// Coarse maturity band derived from a `0..=100` score. Stable label that
/// doesn't wobble on a one-point change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MaturityLevel {
    /// `0..25` — one-liners, vague asks, no grounding.
    Nascent,
    /// `25..50` — some specificity or structure, but thin.
    Developing,
    /// `50..75` — concrete, constrained, decomposed asks.
    Proficient,
    /// `75..90` — consistently rich, well-scoped prompting.
    Advanced,
    /// `90..=100` — exemplary: concrete, bounded, verified, and grounded.
    Exemplary,
}

impl MaturityLevel {
    pub fn from_score(score: f64) -> Self {
        match score {
            s if s >= 90.0 => MaturityLevel::Exemplary,
            s if s >= 75.0 => MaturityLevel::Advanced,
            s if s >= 50.0 => MaturityLevel::Proficient,
            s if s >= 25.0 => MaturityLevel::Developing,
            _ => MaturityLevel::Nascent,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            MaturityLevel::Nascent => "nascent",
            MaturityLevel::Developing => "developing",
            MaturityLevel::Proficient => "proficient",
            MaturityLevel::Advanced => "advanced",
            MaturityLevel::Exemplary => "exemplary",
        }
    }

    /// A small emoji badge for the PR body.
    pub fn emoji(self) -> &'static str {
        match self {
            MaturityLevel::Nascent => "🌱",
            MaturityLevel::Developing => "🪴",
            MaturityLevel::Proficient => "🌿",
            MaturityLevel::Advanced => "🌳",
            MaturityLevel::Exemplary => "🦾",
        }
    }
}

/// A diagnostic flag — *why* a prompt lost points. Diagnostic, never
/// prescriptive (we don't hand engineers a keyword list to stuff). At most two
/// are surfaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flag {
    TooShort,
    Vague,
    WeakContext,
    WeakVerification,
    Repetitive,
    HardToScan,
    /// The prompt is dominated by pasted machine output (log / trace / diff)
    /// with only a thin authored ask around it.
    MostlyPaste,
    /// Constraints / prohibitions but no positive, actionable goal — "don't do
    /// X" with no "do Y". (arXiv 2601.13118 / OpenAI: say what to do.)
    NoObjective,
}

impl Flag {
    pub fn label(self) -> &'static str {
        match self {
            Flag::TooShort => "too short",
            Flag::Vague => "vague / under-specified",
            Flag::WeakContext => "weak context",
            Flag::WeakVerification => "no acceptance criteria",
            Flag::Repetitive => "repetitive",
            Flag::HardToScan => "hard to scan",
            Flag::MostlyPaste => "mostly pasted output",
            Flag::NoObjective => "no clear objective",
        }
    }
}

/// Score for a single prompt.
#[derive(Debug, Clone, PartialEq)]
pub struct PromptScore {
    /// Composite maturity score, `0.0..=100.0`. `0.0` when [`unscored`] is set.
    ///
    /// [`unscored`]: PromptScore::unscored
    pub score: f64,
    pub level: MaturityLevel,
    pub breakdown: PromptScoreBreakdown,
    /// Detected authored-prose language (English or Japanese). Selects the
    /// lexicon/tokenizer/readability path; exposed so callers can label the score.
    pub lang: Lang,
    /// *Authored* word count — pasted artifact lines and code spans are excluded,
    /// so a giant log paste doesn't read as a long prompt. For Japanese this is a
    /// word-equivalent estimate (see [`Features`]).
    pub words: usize,
    /// Up to two diagnostic flags, weakest dimension first.
    pub flags: Vec<Flag>,
    /// When `Some(reason)`, the prompt was **not assessable** as prompt craft
    /// (no authored request, or non-English / unsupported text) and the score is
    /// a placeholder `0.0` that callers should render as "unscored", not as a
    /// bad score. Abstaining beats confidently mis-scoring text the heuristics
    /// don't cover.
    pub unscored: Option<&'static str>,
}

impl PromptScore {
    pub fn is_unscored(&self) -> bool {
        self.unscored.is_some()
    }
}

/// Branch-level roll-up across every AI-commit prompt.
#[derive(Debug, Clone, PartialEq)]
pub struct BranchPromptScore {
    /// Length-weighted mean of the per-prompt scores, `0.0..=100.0`.
    pub score: f64,
    pub level: MaturityLevel,
    /// Component means (length-weighted), for the breakdown table.
    pub breakdown: PromptScoreBreakdown,
    /// Prompts that were scored (non-empty).
    pub scored_prompts: usize,
    /// AI commits considered (denominator for coverage).
    pub ai_commits: usize,
    /// `scored_prompts / ai_commits` — how much of the branch we could measure.
    pub coverage: f64,
    /// True when coverage < 0.8: the score is based on a minority of commits.
    pub low_confidence: bool,
    /// Aggregated diagnostic flags across the branch (most common first, ≤3).
    pub flags: Vec<Flag>,
}

impl BranchPromptScore {
    /// Empty roll-up — caller should usually *skip* rendering rather than show
    /// this; it exists so the function is total.
    pub fn empty() -> Self {
        BranchPromptScore {
            score: 0.0,
            level: MaturityLevel::Nascent,
            breakdown: PromptScoreBreakdown::zero(),
            scored_prompts: 0,
            ai_commits: 0,
            coverage: 0.0,
            low_confidence: false,
            flags: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.scored_prompts == 0
    }
}

// ── Entry points ─────────────────────────────────────────────────────────────

/// Score a single prompt string.
pub fn score_prompt(prompt: &str) -> PromptScore {
    let f = Features::extract(prompt);

    // ── Abstention: refuse to score what the heuristics don't cover ──────────
    // Better an honest "unscored (reason)" than a confident wrong number.
    if let Some(reason) = f.unscored_reason() {
        return PromptScore {
            score: 0.0,
            level: MaturityLevel::Nascent,
            breakdown: PromptScoreBreakdown::zero(),
            lang: f.lang,
            words: f.words,
            flags: Vec::new(),
            unscored: Some(reason),
        };
    }

    let breakdown = f.breakdown();

    // ── Multiplicative core × additive enrichment lift ──────────────────────
    // core ∈ [CORE_MIN^3, 1]; lift ∈ [ENRICHMENT_BASE, 1]. A weak core slot caps
    // the whole score (no balance gate needed); enrichment only lifts an already
    // solid core.
    let core = breakdown.core();
    let lift = ENRICHMENT_BASE + (1.0 - ENRICHMENT_BASE) * ENRICHMENT.lift(&breakdown);
    let mut score = 100.0 * core * lift;

    // (1) Repetition penalty — phrase-farming ("must test format must test
    //     format") is multiplied down.
    score *= f.repetition_factor;

    // (2) Short-prompt floor — under a handful of authored words you can't fully
    //     specify a coding task, however many keywords are packed in.
    if f.words < SHORT_PROMPT_WORDS {
        score = score.min(SHORT_PROMPT_CAP);
    }

    // (3) Evidence bonus — attached machine output is grounding, not craft: a
    //     small fixed credit on top, saturating (never scales with paste
    //     volume) and applied last so a lazy one-liner around a log wall can't
    //     ride the paste into a higher band.
    score += EVIDENCE_BONUS_MAX * breakdown.evidence;

    let score = score.clamp(0.0, 100.0);

    PromptScore {
        level: MaturityLevel::from_score(score),
        flags: f.flags(&breakdown),
        lang: f.lang,
        words: f.words,
        breakdown,
        score,
        unscored: None,
    }
}

/// Score every AI-commit prompt on a branch and roll them up.
///
/// `prompts` is the prompt text of each AI commit (empty strings allowed and
/// ignored). `ai_commits` is the total AI-commit count, used as the coverage
/// denominator — pass `prompts.len()` if every AI commit carried a prompt.
///
/// The branch score is a **length-weighted mean** of the per-prompt scores
/// (weight `= clamp(words, 20, 250)`), so a single rambling prompt can't
/// dominate and a disciplined engineer is rewarded for every crisp ask. Prompts
/// are **never concatenated** — concatenation lets many weak prompts look mature
/// by pooling vocabulary and structure. **Unscored** prompts (no authored
/// request, non-English) are dropped from the mean but still count against
/// coverage.
///
/// The mean is then **shrunk toward a neutral prior** ([`BRANCH_PRIOR_MEAN`],
/// pseudo-count [`BRANCH_PRIOR_STRENGTH`]) so a branch with only one or two
/// prompts can't be crowned or condemned on a tiny sample: three great prompts
/// pull most of the way to their mean, one great prompt only part way.
pub fn score_branch<I, S>(prompts: I, ai_commits: usize) -> BranchPromptScore
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let scored: Vec<PromptScore> = prompts
        .into_iter()
        .filter(|p| !p.as_ref().trim().is_empty())
        .map(|p| score_prompt(p.as_ref()))
        .filter(|s| !s.is_unscored())
        .collect();

    if scored.is_empty() {
        return BranchPromptScore::empty();
    }

    // Length weight, clamped so neither a 3-word ask nor a giant prompt skews
    // the mean. Computed once per prompt and reused across all nine weighted
    // means below (was recomputed per mean, per prompt).
    let weights: Vec<f64> = scored.iter().map(|s| s.words.clamp(20, 250) as f64).collect();
    let total_w: f64 = weights.iter().sum();

    let wmean = |get: &dyn Fn(&PromptScore) -> f64| -> f64 {
        scored.iter().zip(&weights).map(|(s, w)| get(s) * w).sum::<f64>() / total_w
    };
    let wmean_b = |get: &dyn Fn(&PromptScoreBreakdown) -> f64| -> f64 {
        scored.iter().zip(&weights).map(|(s, w)| get(&s.breakdown) * w).sum::<f64>() / total_w
    };

    let breakdown = PromptScoreBreakdown {
        objective: wmean_b(&|b| b.objective),
        grounding: wmean_b(&|b| b.grounding),
        direction: wmean_b(&|b| b.direction),
        context: wmean_b(&|b| b.context),
        examples: wmean_b(&|b| b.examples),
        structure: wmean_b(&|b| b.structure),
        diversity: wmean_b(&|b| b.diversity),
        clarity: wmean_b(&|b| b.clarity),
        adequacy: wmean_b(&|b| b.adequacy),
        evidence: wmean_b(&|b| b.evidence),
        flesch_reading_ease: wmean_b(&|b| b.flesch_reading_ease),
        fk_grade: wmean_b(&|b| b.fk_grade),
        gunning_fog: wmean_b(&|b| b.gunning_fog),
    };
    // Empirical-Bayes shrinkage toward a neutral prior: with `n` scored prompts,
    // pull the length-weighted mean toward BRANCH_PRIOR_MEAN with weight
    // BRANCH_PRIOR_STRENGTH (in prompt-equivalents). Stabilises small samples.
    let raw = wmean(&|s| s.score);
    let n = scored.len() as f64;
    let score = (BRANCH_PRIOR_STRENGTH * BRANCH_PRIOR_MEAN + n * raw)
        / (BRANCH_PRIOR_STRENGTH + n);

    // Coverage / confidence.
    let denom = ai_commits.max(scored.len());
    let coverage = scored.len() as f64 / denom as f64;

    // Aggregate flags: most frequent across prompts, up to three.
    let mut tally: HashMap<&'static str, (usize, Flag)> = HashMap::new();
    for s in &scored {
        for &fl in &s.flags {
            tally.entry(fl.label()).or_insert((0, fl)).0 += 1;
        }
    }
    let mut flag_counts: Vec<(usize, Flag)> = tally.into_values().collect();
    flag_counts.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.label().cmp(b.1.label())));
    let flags: Vec<Flag> = flag_counts.into_iter().take(3).map(|(_, f)| f).collect();

    BranchPromptScore {
        level: MaturityLevel::from_score(score),
        score,
        breakdown,
        scored_prompts: scored.len(),
        ai_commits: denom,
        coverage,
        low_confidence: coverage < 0.8,
        flags,
    }
}

// ── Feature extraction ───────────────────────────────────────────────────────

/// Everything we measure off one prompt. Built once in [`Features::extract`].
/// All fields describe the **authored** portion of the prompt — pasted
/// machine-artifact lines are split off first by [`segment_artifacts`] and
/// only feed `artifact_lines` / the evidence signal.
struct Features {
    /// Detected script of the authored prose. Selects the lexicon set,
    /// tokenizer, and readability handling.
    lang: Lang,
    /// Authored word count (artifact lines and code spans excluded). For
    /// Japanese this is a *word-equivalent* estimate (content chars ÷ 2 + Latin
    /// word runs) so length thresholds and per-100-word densities stay
    /// comparable across scripts.
    words: usize,
    /// Prose tokens (lowercased), code spans removed — for diversity/readability.
    /// Japanese is segmented by character-class run (see [`tokenize_ja`]).
    prose_tokens: Vec<String>,
    sentences: usize,
    syllables: usize,
    polysyllables: usize,
    // ── concreteness / grounding inputs ──
    code_refs: usize,
    quoted: usize,
    numbers: usize,
    action_verbs: usize,
    imprecise_verbs: usize,
    weak_words: usize,
    grounding_refs: usize,
    // ── objective inputs ──
    /// Imperative opener ("Add …", "Fix …") — a positive directive up front.
    imperative_open: bool,
    // ── context inputs ──
    context_markers: usize,
    // ── direction inputs ──
    strong_constraints: usize,
    soft_constraints: usize,
    negative_directives: usize,
    output_shape: usize,
    verification: usize,
    /// A named, runnable acceptance check ("done when `cargo test` passes").
    executable_acceptance: bool,
    preconditions: usize,
    postconditions: usize,
    exceptions: usize,
    edge_cases: usize,
    safety: usize,
    scope: usize,
    ambiguous_cond: usize,
    // ── examples inputs ──
    example_markers: usize,
    arrows: usize,
    // ── structure inputs ──
    bullets: usize,
    numbered: usize,
    headings: usize,
    code_fences: usize,
    // ── anti-gaming ──
    repetition_factor: f64,
    /// Fraction of authored prose tokens that are English function words — the
    /// natural-language discriminator (NLoN). Drives non-English abstention.
    stopword_ratio: f64,
    // ── artifact / evidence inputs ──
    /// Lines classified as pasted machine output (logs, traces, diffs, fenced
    /// block interiors).
    artifact_lines: usize,
    /// Non-blank lines that remained authored.
    authored_lines: usize,
    /// Deictic references from the authored prose to the paste ("the error
    /// below", "this log", …).
    evidence_refs: usize,
}

impl Features {
    fn extract(text: &str) -> Self {
        // Split pasted machine artifacts (logs, stack traces, diffs, fenced
        // blocks) from the authored prose first — everything below measures
        // only what the engineer actually *wrote*.
        let seg = segment_artifacts(text);
        let text: &str = &seg.authored;
        let lang = detect_lang(text);
        // Mask code/paths/URLs so prose metrics aren't corrupted, but keep the
        // raw text for code-ref counting and lexicon matching.
        let masked = mask_code(text);
        // Tokenise for diversity/repetition: Japanese by character-class run
        // (no spaces), everything else by word.
        let prose_tokens = match lang {
            Lang::Japanese => tokenize_ja(&masked),
            Lang::English => tokenize_words(&masked),
        };
        // Word count. English: prose tokens. Japanese: a word-equivalent estimate
        // so length gating and per-100-word densities stay comparable.
        let words = match lang {
            Lang::Japanese => ja_word_equiv(&masked),
            Lang::English => prose_tokens.len(),
        };
        // English readability inputs (syllables). Japanese uses a char-based
        // proxy in `breakdown`, so these stay zero for JA.
        let (mut syllables, mut polysyllables) = (0usize, 0usize);
        if lang == Lang::English {
            for w in &prose_tokens {
                let s = count_syllables(w);
                syllables += s;
                if s >= 3 {
                    polysyllables += 1;
                }
            }
        }
        let sentences = count_sentences(&masked).max(1);
        let (bullets, numbered, headings, code_fences) = count_structure(text);

        let lower = text.to_ascii_lowercase();
        let word_set: HashSet<&str> = prose_tokens.iter().map(|s| s.as_str()).collect();
        // Word-occurrence map over the raw lowercased text, built once and shared
        // by every English single-word lexicon lookup below.
        let lower_counts = word_counts(&lower);

        // Language-dispatched lexicon hit: English uses the word-gated counter;
        // Japanese counts substring occurrences (a space-less script has no word
        // boundaries, so substring matching is the natural fit).
        let hit = |en: &[&str], ja: &[&str]| -> usize {
            match lang {
                Lang::Japanese => ja_hits(&lower, ja),
                Lang::English => lexicon_hits(&lower, &lower_counts, &word_set, en),
            }
        };

        // Function-word ratio (NLoN's NL discriminator) — English only; drives
        // the non-supported-language abstention. Japanese is scored, so it is
        // exempt (reported as fully "natural language").
        let stopword_ratio = match lang {
            Lang::Japanese => 1.0,
            Lang::English if prose_tokens.is_empty() => 0.0,
            Lang::English => {
                prose_tokens.iter().filter(|w| LINE_STOPWORDS.contains(&w.as_str())).count() as f64
                    / prose_tokens.len() as f64
            }
        };

        Features {
            lang,
            code_refs: count_code_refs(text),
            quoted: text.matches('`').count() / 2 + text.matches('"').count() / 2,
            numbers: prose_tokens
                .iter()
                .filter(|w| w.chars().any(|c| c.is_ascii_digit()))
                .count(),
            action_verbs: hit(ACTION_VERBS, JA_ACTION_VERBS),
            imprecise_verbs: hit(IMPRECISE_VERBS, JA_IMPRECISE_VERBS),
            weak_words: hit(WEAK_WORDS, JA_WEAK_WORDS),
            grounding_refs: hit(GROUNDING_REFS, JA_GROUNDING_REFS),
            imperative_open: match lang {
                Lang::Japanese => ja_hits(&lower, JA_IMPERATIVE) > 0,
                Lang::English => opens_with_imperative(text),
            },
            context_markers: hit(CONTEXT_MARKERS, JA_CONTEXT_MARKERS),
            strong_constraints: hit(STRONG_CONSTRAINTS, JA_STRONG_CONSTRAINTS),
            soft_constraints: hit(SOFT_CONSTRAINTS, JA_SOFT_CONSTRAINTS),
            negative_directives: hit(NEGATIVE_DIRECTIVES, JA_NEGATIVE_DIRECTIVES),
            output_shape: hit(OUTPUT_SHAPE, JA_OUTPUT_SHAPE),
            verification: hit(VERIFICATION, JA_VERIFICATION),
            executable_acceptance: lower_matches_any(&lower, RUNNERS)
                || (lang == Lang::Japanese && ja_hits(&lower, JA_RUNNERS) > 0),
            preconditions: hit(PRECONDITIONS, JA_PRECONDITIONS),
            postconditions: hit(POSTCONDITIONS, JA_POSTCONDITIONS),
            exceptions: hit(EXCEPTIONS, JA_EXCEPTIONS),
            edge_cases: hit(EDGE_CASES, JA_EDGE_CASES),
            safety: hit(SAFETY, JA_SAFETY),
            scope: hit(SCOPE, JA_SCOPE),
            ambiguous_cond: hit(AMBIGUOUS_COND, JA_AMBIGUOUS_COND),
            example_markers: hit(EXAMPLE_MARKERS, JA_EXAMPLE_MARKERS),
            arrows: count_arrows(text),
            evidence_refs: hit(EVIDENCE_REFS, JA_EVIDENCE_REFS),
            artifact_lines: seg.artifact_lines,
            authored_lines: seg.authored_lines,
            repetition_factor: repetition_factor(&prose_tokens),
            stopword_ratio,
            bullets,
            numbered,
            headings,
            code_fences,
            words,
            sentences,
            syllables,
            polysyllables,
            prose_tokens,
        }
    }

    fn breakdown(&self) -> PromptScoreBreakdown {
        let n = self.words.max(1) as f64;
        let per100 = |count: usize| (count as f64) / n * 100.0;

        // Vagueness penalty from the NALABS/Femmer requirements-smell lexicon:
        // density of weak words (per 100 words) is a negative signal shared by
        // objective and grounding.
        let vagueness = (per100(self.weak_words) / 8.0).clamp(0.0, 1.0);

        // ── Core slot: objective — is there a positive, actionable goal? ─────
        // A single clear action verb earns most of the credit; a second verb or
        // an imperative opener tops it up. Leading with the problem statement
        // ("The parser miscounts…") must NOT be penalised, so the opener is a
        // bonus, not a requirement. Dragged down by vagueness; a prompt of pure
        // prohibitions (no action verb) scores ~0 here, which the multiplicative
        // core then turns into a real cap.
        // Imprecise-verb penalty (Paska's "not precise verb" smell): a prompt
        // whose actionable content is dominated by verbs like "handle" /
        // "process" / "support" names no real action.
        let imprecise = (per100(self.imprecise_verbs) / 6.0).clamp(0.0, 1.0);
        let objective = {
            let base = if self.action_verbs == 0 {
                0.0
            } else {
                0.6 + 0.2 * f64::from(self.action_verbs >= 2)
                    + 0.2 * f64::from(self.imperative_open)
            };
            (base - 0.5 * vagueness - 0.4 * imprecise).clamp(0.0, 1.0)
        };

        // ── Core slot: grounding — concrete references ──────────────────────
        // Paths, `func()`, idents, quoted spans, numbers, and grounding nouns.
        let grounding = (0.42 * cap_ratio(self.code_refs, 3)
            + 0.18 * cap_ratio(self.quoted, 2)
            + 0.12 * cap_ratio(self.numbers, 3)
            + 0.28 * cap_ratio(self.grounding_refs, 2)
            - 0.4 * vagueness)
            .clamp(0.0, 1.0);

        // ── Core slot: direction — did they bound the agent? ────────────────
        // Acceptance (a runnable check beats a bare "ensure"), constraints
        // (must-class weighted over should-class), and the behavioral contract
        // (I/O format · post- · pre-conditions · exceptions — weights seeded
        // from arXiv 2601.13118 prevalence). "Otherwise"-style ambiguous
        // conditionals dock a little.
        let acceptance = (0.6 * cap_ratio(self.verification, 3)
            + 0.4 * f64::from(self.executable_acceptance))
        .clamp(0.0, 1.0);
        let constraint_sig = (0.7 * cap_ratio(self.strong_constraints, 3)
            + 0.3 * cap_ratio(self.soft_constraints, 2)
            + 0.15 * cap_ratio(self.scope, 2))
        .clamp(0.0, 1.0);
        let contract = (0.36 * cap_ratio(self.output_shape, 3)
            + 0.24 * cap_ratio(self.postconditions, 2)
            + 0.20 * cap_ratio(self.preconditions, 2)
            + 0.20 * cap_ratio(self.exceptions + self.edge_cases + self.safety, 2))
        .clamp(0.0, 1.0);
        let ambiguity = (0.15 * cap_ratio(self.ambiguous_cond, 2)).clamp(0.0, 1.0);
        let direction = (0.42 * acceptance + 0.34 * constraint_sig + 0.24 * contract
            - ambiguity)
            .clamp(0.0, 1.0);

        // ── Enrichment: context grounding (background / why) ────────────────
        let context = cap_ratio(self.context_markers, 4).clamp(0.0, 1.0);

        // ── Enrichment: examples / illustrations ────────────────────────────
        let examples = (0.6 * cap_ratio(self.example_markers, 2)
            + 0.4 * cap_ratio(self.arrows, 2))
        .clamp(0.0, 1.0);

        // ── Structure / decomposition ───────────────────────────────────────
        let multi_sentence = cap_ratio(self.sentences.saturating_sub(1), 3);
        let structure = (0.35 * cap_ratio(self.bullets + self.numbered, 4)
            + 0.20 * cap_ratio(self.headings, 2)
            + 0.20 * cap_ratio(self.code_fences, 2)
            + 0.25 * multi_sentence)
        .clamp(0.0, 1.0);

        // ── Diversity — adaptive MATTR over prose tokens ────────────────────
        let diversity = lexical_diversity(&self.prose_tokens);

        // ── Clarity — readability band on code-masked prose ─────────────────
        // English uses the Flesch/FK/Fog indices. Those are English-specific
        // (syllable-based), so Japanese instead uses a sentence-length proxy —
        // the dominant driver of Japanese readability — and leaves the English
        // indices at 0 for display.
        let words_per_sentence = n / self.sentences as f64;
        let (fk_grade, flesch_reading_ease, gunning_fog, clarity) = match self.lang {
            Lang::English => {
                let syll_per_word = self.syllables as f64 / n;
                let fk = 0.39 * words_per_sentence + 11.8 * syll_per_word - 15.59;
                let fre = 206.835 - 1.015 * words_per_sentence - 84.6 * syll_per_word;
                let fog = 0.4 * (words_per_sentence + 100.0 * self.polysyllables as f64 / n);
                let clar = clarity_band(fk, fre, self.words);
                (fk, fre, fog, clar)
            }
            Lang::Japanese => (0.0, 0.0, 0.0, ja_clarity(words_per_sentence, self.words)),
        };

        // ── Adequacy — length sweet spot (additive, gentle) ─────────────────
        let adequacy = length_adequacy(self.words);

        // ── Evidence — attached artifact, saturating ────────────────────────
        let evidence = self.evidence_signal();

        PromptScoreBreakdown {
            objective,
            grounding,
            direction,
            context,
            examples,
            structure,
            diversity,
            clarity,
            adequacy,
            evidence,
            flesch_reading_ease,
            fk_grade,
            gunning_fog,
        }
    }

    /// Evidence signal in `0.0..=1.0`. Binary-ish and saturating by design:
    /// *having* an artifact attached is worth 0.7; explicitly framing it from
    /// the prose ("the error below", "this log") tops it up to 1.0. Volume is
    /// deliberately not a factor — see the module docs on paste-gaming.
    fn evidence_signal(&self) -> f64 {
        if self.artifact_lines == 0 {
            return 0.0;
        }
        0.7 + 0.3 * cap_ratio(self.evidence_refs, 2)
    }

    /// Reason to **abstain** from scoring, or `None` if the prompt is assessable.
    /// Narrow by design — very short English/Japanese asks stay scored (and score
    /// low with advice); we only refuse text the heuristics genuinely don't
    /// cover. English and Japanese are supported; other languages abstain.
    fn unscored_reason(&self) -> Option<&'static str> {
        if self.words == 0 {
            // Pure paste, punctuation, or empty — no authored request to assess.
            return Some("no authored request");
        }
        // Japanese is exempt (stopword_ratio is forced to 1.0 for it). Otherwise,
        // substantial text with almost no English function words is neither
        // English nor Japanese (or is code-soup that survived masking): the
        // lexicon/readability signals are meaningless on it, so abstain.
        if self.words >= 8 && self.stopword_ratio < 0.05 {
            return Some("unsupported language");
        }
        None
    }

    /// Up to two diagnostic flags, weakest qualifying dimension first.
    fn flags(&self, b: &PromptScoreBreakdown) -> Vec<Flag> {
        let mut out = Vec::new();
        if self.words < 15 {
            out.push(Flag::TooShort);
        }
        // A wall of pasted output around a thin ask: enough artifact lines to
        // dominate (≥5 and ≥3× the authored lines) with under 40 authored
        // words. Diagnostic: "write the ask, don't just paste".
        if self.artifact_lines >= 5
            && self.words < 40
            && self.artifact_lines >= 3 * self.authored_lines.max(1)
        {
            out.push(Flag::MostlyPaste);
        }
        // Prohibitions with no positive goal: constraints present but objective
        // essentially absent. Surfaced ahead of the generic weak-signal flags.
        if b.objective < 0.2 && self.negative_directives >= 1 && out.len() < 2 {
            out.push(Flag::NoObjective);
        }
        // Candidate (signal, flag) pairs, lowest signal surfaced first. The core
        // slots map to the actionable flags; grounding+objective share "vague".
        let mut cands: Vec<(f64, Flag)> = vec![
            (b.objective.max(b.grounding), Flag::Vague),
            (b.context, Flag::WeakContext),
            (b.direction, Flag::WeakVerification),
        ];
        if self.repetition_factor < 0.9 {
            cands.push((b.diversity.min(self.repetition_factor), Flag::Repetitive));
        }
        if self.words >= 40 {
            cands.push((b.clarity, Flag::HardToScan));
        }
        cands.sort_by(|x, y| x.0.partial_cmp(&y.0).unwrap_or(std::cmp::Ordering::Equal));
        for (sig, fl) in cands {
            if out.len() >= 2 {
                break;
            }
            if sig < 0.35 && !out.contains(&fl) {
                out.push(fl);
            }
        }
        out.truncate(2);
        out
    }
}

// ── Lexicons ─────────────────────────────────────────────────────────────────
//
// All lowercase. Single-word entries match whole prose tokens; multi-word
// entries match as substrings of the lowercased raw text. Each category is
// capped in the breakdown, so stuffing one list can't farm the score.

const ACTION_VERBS: &[&str] = &[
    "implement", "fix", "refactor", "add", "remove", "delete", "update", "design",
    "build", "create", "write", "migrate", "debug", "optimize", "rename", "extract",
    "replace", "wire", "integrate", "parse", "render", "validate", "edit",
    "convert", "move", "split", "guard", "harden",
    "sanitize", "reject", "return", "cache", "expose", "document", "cover", "port",
    "upgrade", "patch", "register", "normalize", "serialize", "deserialize", "strip",
    "compute", "check", "enforce", "apply", "disable", "enable", "configure",
    "extend", "adjust", "drop", "wrap", "cap", "emit", "escape", "trim", "raise",
];

/// Verbs that *look* actionable but name no precise action — Paska's
/// "not precise verb" smell (Veizaga et al., arXiv 2305.07097). A prompt whose
/// only "action" is one of these ("handle the errors", "process the data",
/// "support pagination") is not a clear objective, so their density is a
/// *negative* signal on `objective` — never counted as an [`ACTION_VERBS`] hit.
// NB: "do" and "make" are deliberately excluded — they fire on "do not change"
// and "make sure" (a constraint and an acceptance phrase), not on an imprecise
// action. We keep only verbs that are almost always imprecise *actions*.
const IMPRECISE_VERBS: &[&str] = &[
    "handle", "support", "process", "manage", "deal", "perform", "consider",
    "accomplish", "propose", "ensure", "improve", "address",
];

/// NALABS / Femmer "requirements smells" — vague, subjective, or non-actionable
/// words. Their density is a *negative* specificity signal.
const WEAK_WORDS: &[&str] = &[
    "appropriate", "adequate", "etc", "some", "various", "several", "fast",
    "slow", "better", "nice", "clean", "good", "robust", "flexible", "efficient",
    "reasonable", "normal", "user-friendly", "easy", "simple", "properly",
    "correctly", "somehow", "stuff", "thing", "things", "maybe", "probably",
    "as possible", "as needed", "and so on", "or something", "if needed",
];

const CONTEXT_MARKERS: &[&str] = &[
    "because", "currently", "existing", "current", "repo", "repository", "codebase",
    "background", "goal", "before", "after", "context", "given", "right now",
    "at the moment", "the problem", "we have", "there is", "it is", "today",
    "previously", "legacy", "so that",
];

const GROUNDING_REFS: &[&str] = &[
    "file", "module", "function", "method", "struct", "class", "command", "cli",
    "endpoint", "test", "branch", "directory", "line", "the user", "reviewer",
    "manager",
];

/// Strong (imperative) constraints — "must"-class. Weighted above the soft
/// class in `direction` (arXiv 2601.13118 / OpenAI: prefer assertive language).
const STRONG_CONSTRAINTS: &[&str] = &[
    "must", "only", "without", "avoid", "never", "always", "do not", "don't",
    "dont", "at least", "at most", "no more than", "limit", "require", "required",
    "offline", "backward", "compatible", "minimal",
];

/// Soft constraints — "should"-class, hedged. Real but weaker bounding.
const SOFT_CONSTRAINTS: &[&str] = &[
    "should", "prefer", "preferably", "try to", "ideally", "keep", "preserve",
];

/// Negative directives — the "don't do X" prohibitions. Used to detect
/// prohibitions-without-objective (the `NoObjective` flag).
const NEGATIVE_DIRECTIVES: &[&str] =
    &["without", "avoid", "never", "do not", "don't", "dont", "no unrelated", "don't change"];

/// Pre-conditions — assumptions that must hold on the input before execution.
const PRECONDITIONS: &[&str] = &[
    "precondition", "assume", "assumes", "assuming", "given that", "input is",
    "when the input", "must be non-empty", "expects", "requires that", "invariant",
];

/// Post-conditions — guarantees on the output/result after execution.
const POSTCONDITIONS: &[&str] = &[
    "postcondition", "should return", "must return", "returns", "result is",
    "guarantee", "guarantees", "ensures that", "resulting", "so that the result",
    "the output should",
];

/// Exception / error-handling behaviour the agent must specify.
const EXCEPTIONS: &[&str] = &[
    "raise", "throw", "throws", "exception", "panic", "error case", "on error",
    "fail with", "return an error", "handle the error", "err(", "result<",
];

/// Ambiguous conditionals — the "otherwise"-style smell (arXiv 2601.13118):
/// a branch that refers to an unstated second condition.
const AMBIGUOUS_COND: &[&str] = &["otherwise", "or else", "if not", "as appropriate"];

/// Example / illustration markers.
const EXAMPLE_MARKERS: &[&str] = &[
    "for example", "e.g.", "example:", "examples:", "for instance", "such as",
    "sample", "doctest", "input:", "output:", "expected output", "given input",
];

/// Runnable acceptance checks — a named command the agent can execute to know
/// it is done. A far stronger acceptance signal than the word "ensure".
const RUNNERS: &[&str] = &[
    "cargo test", "cargo build", "cargo clippy", "npm test", "npm run", "yarn ",
    "pytest", "go test", "make ", "./", "mvn ", "gradle", "done when", "done-when",
    "acceptance criteria", "ci passes", "the test passes", "tests pass",
];

const OUTPUT_SHAPE: &[&str] = &[
    "format", "json", "yaml", "markdown", "table", "section", "body", "schema",
    "signature", "return", "output", "interface", "shape", "field", "column",
];

const VERIFICATION: &[&str] = &[
    "test", "tests", "verify", "validate", "assert", "expect", "ensure", "pass",
    "passing", "coverage", "acceptance", "done when", "done-when", "given", "when",
    "then", "benchmark", "regression",
];

const EDGE_CASES: &[&str] = &[
    "edge case", "edge-case", "boundary", "corner case", "empty", "null", "none",
    "overflow", "off-by-one", "race", "concurrent", "unicode", "negative",
];

const SAFETY: &[&str] = &[
    "security", "privacy", "secret", "credential", "token", "auth", "sanitize",
    "injection", "rate limit", "api limit", "permission",
];

const SCOPE: &[&str] = &[
    "only", "scope", "do not change", "don't change", "no unrelated", "out of scope",
    "in scope", "leave", "untouched", "just",
];

/// Deictic references from the authored prose to an attached artifact — the
/// difference between *framing* a paste ("the trace below shows…") and just
/// dumping it. Tops up the evidence signal; matched on authored text only.
const EVIDENCE_REFS: &[&str] = &[
    "below", "above", "following", "attached", "pasted", "paste", "this error",
    "the error", "this log", "the log", "this output", "the output", "the trace",
    "the backtrace", "the stack trace", "this diff", "the diff", "stderr",
    "stdout", "error message", "test output", "the failure", "this failure",
];

// ── Japanese lexicons (substring-matched; see `ja_hits`) ─────────────────────
//
// Parallel to the English lexicons above, one per slot signal. Entries are kanji
// compounds or grammatical markers chosen to be specific enough that incidental
// substring collisions are rare. Known limitation: because matching is by
// substring, a precise action stem (e.g. 変更 "modify") also matches inside a
// prohibition (変更しない "do not modify"), so a pure-prohibition Japanese prompt
// can score a little objective credit it wouldn't in English. Acceptable for a
// dictionary-free path; a morphological analyser would resolve it.

const JA_ACTION_VERBS: &[&str] = &[
    "実装", "修正", "追加", "削除", "変更", "作成", "生成", "更新", "リファクタ",
    "リネーム", "抽出", "置換", "統合", "移行", "最適化", "解析", "描画", "導入",
    "定義", "分割", "拡張", "実行",
];

const JA_IMPRECISE_VERBS: &[&str] =
    &["対応", "処理", "サポート", "管理", "改善", "考慮", "検討", "対処"];

const JA_WEAK_WORDS: &[&str] = &[
    "適切", "適宜", "ちゃんと", "きちんと", "いい感じ", "綺麗", "柔軟", "効率的",
    "簡単", "シンプル", "何とか", "なるべく", "うまく", "しっかり", "正しく", "など",
];

const JA_GROUNDING_REFS: &[&str] = &[
    "関数", "メソッド", "ファイル", "モジュール", "クラス", "構造体", "引数",
    "戻り値", "変数", "ディレクトリ", "コマンド", "エンドポイント", "ブランチ",
    "インターフェース", "フィールド", "定数",
];

/// Request / imperative markers (verb-final in Japanese, so this is not an
/// "opener" but a whole-text signal that a directive was actually made).
const JA_IMPERATIVE: &[&str] = &[
    "ください", "て下さい", "せよ", "なさい", "すること", "ましょう", "してほしい",
    "お願い",
];

const JA_CONTEXT_MARKERS: &[&str] = &[
    "現在", "既存", "現状", "背景", "目的", "なぜなら", "理由", "以前", "問題",
    "課題", "レガシー", "従来", "現行",
];

const JA_STRONG_CONSTRAINTS: &[&str] = &[
    "必ず", "必須", "してはいけない", "してはならない", "しないで", "禁止", "のみ",
    "だけ", "決して", "常に", "絶対",
];

const JA_SOFT_CONSTRAINTS: &[&str] =
    &["べき", "望ましい", "できれば", "推奨", "維持", "保持"];

const JA_NEGATIVE_DIRECTIVES: &[&str] = &[
    "してはいけない", "してはならない", "しないで", "禁止", "避ける", "変更しない",
    "触らない",
];

const JA_OUTPUT_SHAPE: &[&str] = &[
    "フォーマット", "形式", "テーブル", "スキーマ", "シグネチャ", "出力", "構造",
    "json", "yaml", "markdown",
];

const JA_VERIFICATION: &[&str] = &[
    "テスト", "検証", "確認", "アサート", "通る", "パス", "合格", "カバレッジ",
    "回帰", "受け入れ",
];

const JA_RUNNERS: &[&str] =
    &["テストが通", "テストをパス", "テストが成功", "ビルドが通", "成功すること"];

const JA_PRECONDITIONS: &[&str] = &["前提", "仮定", "事前条件", "入力が"];

const JA_POSTCONDITIONS: &[&str] = &["事後条件", "を返す", "結果は", "保証", "出力は"];

const JA_EXCEPTIONS: &[&str] = &["例外", "エラー", "失敗", "パニック", "異常"];

const JA_EDGE_CASES: &[&str] = &[
    "エッジケース", "境界", "空文字", "境界値", "特殊ケース", "オーバーフロー",
    "競合", "並行",
];

const JA_SAFETY: &[&str] = &[
    "セキュリティ", "脆弱性", "認証", "秘密", "資格情報", "サニタイズ",
    "インジェクション", "権限", "プライバシー",
];

const JA_SCOPE: &[&str] = &["範囲", "対象外", "限定", "スコープ"];

const JA_AMBIGUOUS_COND: &[&str] = &["それ以外", "そうでなければ", "場合によって"];

const JA_EXAMPLE_MARKERS: &[&str] =
    &["例えば", "たとえば", "例:", "サンプル", "具体例", "入力例", "出力例"];

const JA_EVIDENCE_REFS: &[&str] = &[
    "以下", "上記", "添付", "貼り付け", "このエラー", "このログ", "スタックトレース",
    "エラーメッセージ", "下記",
];

// ── Slot detector helpers ────────────────────────────────────────────────────

/// True if any entry (a phrase or word, matched as a substring) is present in
/// the lowercased text. For multi-token acceptance/runner phrases.
fn lower_matches_any(lower: &str, lex: &[&str]) -> bool {
    lex.iter().any(|e| lower.contains(e))
}

/// Does the prompt (or one of its first bulleted/numbered lines) open with an
/// imperative action verb? A positive directive up front — "Add …", "Fix …",
/// "Refactor …" — is the clearest signal of a stated objective.
fn opens_with_imperative(text: &str) -> bool {
    for raw in text.lines().take(6) {
        let l = raw.trim_start();
        // Strip a leading bullet / number marker so "1. Add the struct" counts.
        let l = l
            .trim_start_matches(|c: char| {
                matches!(c, '-' | '*' | '•' | '+' | '#' | '.' | ')' | '(')
                    || c.is_ascii_digit()
                    || c.is_whitespace()
            });
        let first = l.split(|c: char| !c.is_ascii_alphabetic()).next().unwrap_or("");
        if !first.is_empty() && ACTION_VERBS.contains(&first.to_ascii_lowercase().as_str()) {
            return true;
        }
    }
    false
}

/// Count input→output arrows (`->`, `=>`, `⇒`) — a compact example/spec form.
fn count_arrows(text: &str) -> usize {
    text.matches("->").count() + text.matches("=>").count() + text.matches('⇒').count()
}

// ── Language detection & Japanese support ────────────────────────────────────
//
// The scorer is script-aware. English is the default; Japanese gets a dedicated
// path — dictionary-free and fully offline, consistent with the module contract
// (no morphological analyser / MeCab dependency). Japanese is detected by kana,
// tokenised by character-class run, and matched against parallel `JA_*` lexicons
// by substring (a space-less script has no word boundaries). Code references,
// structure, numbers, and arrows are script-agnostic and shared. Readability
// indices are English-specific, so `breakdown` uses a character-based proxy for
// Japanese instead of emitting a meaningless Flesch score. Everything else — the
// slot rubric, multiplicative core, guards, aggregation — is language-neutral:
// it consumes feature *counts*, so it works unchanged for either script.

/// Detected authored-prose script.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    English,
    Japanese,
}

fn is_hiragana(c: char) -> bool {
    ('\u{3040}'..='\u{309F}').contains(&c)
}
fn is_katakana(c: char) -> bool {
    ('\u{30A0}'..='\u{30FF}').contains(&c) || ('\u{FF66}'..='\u{FF9D}').contains(&c)
}
fn is_kanji(c: char) -> bool {
    ('\u{4E00}'..='\u{9FFF}').contains(&c) || ('\u{3400}'..='\u{4DBF}').contains(&c)
}
fn is_ja_char(c: char) -> bool {
    is_hiragana(c) || is_katakana(c) || is_kanji(c)
}

/// Classify by script. Kana (hiragana/katakana) is unambiguously Japanese, so
/// its presence — beyond an incidental stray char — selects the Japanese path.
/// Kanji alone stays English (it could be Chinese, and code identifiers never
/// contain kana). Threshold: at least 3 kana characters.
fn detect_lang(text: &str) -> Lang {
    let kana = text.chars().filter(|&c| is_hiragana(c) || is_katakana(c)).count();
    if kana >= 3 {
        Lang::Japanese
    } else {
        Lang::English
    }
}

/// Segment Japanese text into character-class runs: each maximal run of one
/// class (kanji / hiragana / katakana / Latin-alphanumeric) is one token.
/// Dictionary-free and deterministic — coarser than morphological analysis, but
/// enough for lexical-diversity and repetition signals. Punctuation and
/// whitespace are boundaries and dropped.
fn tokenize_ja(text: &str) -> Vec<String> {
    #[derive(PartialEq, Clone, Copy)]
    enum Class {
        Kanji,
        Hira,
        Kata,
        Latin,
    }
    fn class_of(c: char) -> Option<Class> {
        if is_kanji(c) {
            Some(Class::Kanji)
        } else if is_hiragana(c) {
            Some(Class::Hira)
        } else if is_katakana(c) {
            Some(Class::Kata)
        } else if c.is_ascii_alphanumeric() {
            Some(Class::Latin)
        } else {
            None
        }
    }
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut cur_class: Option<Class> = None;
    for ch in text.chars() {
        match class_of(ch) {
            Some(cl) if Some(cl) == cur_class => cur.push(ch.to_ascii_lowercase()),
            Some(cl) => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                cur.push(ch.to_ascii_lowercase());
                cur_class = Some(cl);
            }
            None => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                cur_class = None;
            }
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Word-equivalent size of Japanese text: content characters (kana + kanji) ÷ 2
/// — Japanese words average ~2 characters — plus each Latin-alphanumeric run as
/// one word. Keeps length thresholds and per-100-word densities on a comparable
/// scale to English.
fn ja_word_equiv(masked: &str) -> usize {
    let ja_chars = masked.chars().filter(|&c| is_ja_char(c)).count();
    let mut latin_runs = 0usize;
    let mut in_latin = false;
    for c in masked.chars() {
        if c.is_ascii_alphanumeric() {
            if !in_latin {
                latin_runs += 1;
                in_latin = true;
            }
        } else {
            in_latin = false;
        }
    }
    ja_chars / 2 + latin_runs
}

/// Count Japanese lexicon hits: total substring occurrences of each entry. No
/// word-boundary gating (Japanese has none); entries are chosen to be specific
/// enough (kanji compounds, grammatical markers) that incidental collisions are
/// rare.
fn ja_hits(lower: &str, lex: &[&str]) -> usize {
    lex.iter().map(|e| lower.matches(e).count()).sum()
}

// ── Artifact segmentation ────────────────────────────────────────────────────
//
// Line-level split of a prompt into authored prose and pasted machine output,
// following the NL-vs-machine-text literature (NLoN, infoZilla — see module
// docs): high-precision *strong* patterns (stack frames, compiler diagnostics,
// test-runner lines, log levels, timestamps, diff markers) seed artifact
// blocks; cheap *weak* machine-ish signals (stopword-free, symbol-dense,
// deeply indented, code-token-majority lines) join a block only by contagion
// with an adjacent artifact line, so an isolated technical sentence in prose
// stays authored. Fenced ``` interiors are artifact by construction (the
// fence markers themselves stay authored so structure credit survives).
// Fails toward *authored*: a missed artifact line costs a little noise, an
// eaten prose line costs real signal.

/// Result of [`segment_artifacts`].
struct Segmented {
    /// The authored lines, rejoined with `\n`.
    authored: String,
    /// Lines classified as pasted machine output.
    artifact_lines: usize,
    /// Non-blank lines that stayed authored.
    authored_lines: usize,
}

/// Split `text` into authored prose and pasted machine artifacts.
fn segment_artifacts(text: &str) -> Segmented {
    let lines: Vec<&str> = text.lines().collect();
    let n = lines.len();
    let mut strength = vec![0u8; n];
    let mut fence_marker = vec![false; n];
    let mut in_fence = false;
    for (i, l) in lines.iter().enumerate() {
        if l.trim_start().starts_with("```") {
            in_fence = !in_fence;
            fence_marker[i] = true; // marker stays authored (structure credit)
            continue;
        }
        strength[i] = if in_fence { 2 } else { line_artifact_strength(l) };
    }

    // Strong lines are artifact; weak lines join by contagion with a
    // neighbouring artifact line (forward then backward pass, so runs grow
    // from strong seeds in both directions).
    let mut artifact: Vec<bool> = strength.iter().map(|&s| s >= 2).collect();
    for i in 0..n {
        if strength[i] == 1 && i > 0 && artifact[i - 1] {
            artifact[i] = true;
        }
    }
    for i in (0..n).rev() {
        if strength[i] == 1 && i + 1 < n && artifact[i + 1] {
            artifact[i] = true;
        }
    }

    let mut authored = String::with_capacity(text.len());
    let mut artifact_lines = 0usize;
    let mut authored_lines = 0usize;
    for (i, l) in lines.iter().enumerate() {
        if artifact[i] && !fence_marker[i] {
            artifact_lines += 1;
            continue;
        }
        if !authored.is_empty() {
            authored.push('\n');
        }
        authored.push_str(l);
        if !l.trim().is_empty() && !fence_marker[i] {
            authored_lines += 1;
        }
    }
    Segmented { authored, artifact_lines, authored_lines }
}

/// Classify one line: `2` = unmistakable machine output (seeds an artifact
/// block on its own), `1` = machine-ish (artifact only next to one), `0` =
/// authored.
fn line_artifact_strength(line: &str) -> u8 {
    let t = line.trim();
    if t.is_empty() {
        return 0;
    }
    let lower = t.to_ascii_lowercase();

    // ── Strong: high-precision machine-output patterns ──────────────────────
    // Stack frames: "at src/…" / "at pkg.Class.method(File.java:123)" /
    // 'File "x.py", line N' / numbered Rust/gdb frames.
    if lower.starts_with("at ") && (t.contains('/') || t.contains("::") || t.contains('(')) {
        return 2;
    }
    if t.starts_with("File \"") || is_frame_line(t) {
        return 2;
    }
    // Panics / tracebacks / exception headlines.
    if lower.contains("panicked at")
        || lower.contains("stack backtrace")
        || lower.contains("rust_backtrace")
        || lower.starts_with("traceback (")
        || lower.starts_with("caused by:")
        || lower.starts_with("exception in thread")
        || is_exception_headline(t)
    {
        return 2;
    }
    // Compiler / tool diagnostics.
    if lower.starts_with("error:")
        || lower.starts_with("error[")
        || lower.starts_with("warning:")
        || lower.starts_with("fatal:")
        || t.starts_with("-->")
        || t.contains("npm ERR!")
    {
        return 2;
    }
    if lower.contains("expected") && lower.contains("found") && t.contains('`') {
        return 2; // rustc "expected `X`, found `Y`"
    }
    if lower.starts_with("assertion") && lower.contains("fail") {
        return 2; // "assertion `left == right` failed" / "assertion failed: …"
    }
    if lower.starts_with("left:") || lower.starts_with("right:") {
        return 2; // rustc assert_eq! operand dump
    }
    // Test-runner output.
    if lower.starts_with("test ")
        && (lower.ends_with("... ok")
            || lower.contains("... failed")
            || lower.contains("... ignored"))
    {
        return 2;
    }
    if (lower.starts_with("running ") && (lower.ends_with(" tests") || lower.ends_with(" test")))
        || t == "failures:"
        || t.starts_with("----")
        || t.starts_with("====")
    {
        return 2;
    }
    // Log lines: leading timestamp or log-level token.
    if starts_with_timestamp(t) || starts_with_log_level(t) {
        return 2;
    }
    // Unified diff markers.
    if t.starts_with("@@") || t.starts_with("+++ ") || t.starts_with("--- ")
        || t.starts_with("diff --git")
    {
        return 2;
    }

    // ── Weak: machine-ish (NLoN-style cheap features) ───────────────────────
    // Deep indentation (continuation lines of dumps / assertion diffs).
    if line.starts_with("      ") || line.starts_with('\t') {
        return 1;
    }
    // Diff content lines: +/- glued to content ("- item" bullets keep a space).
    if t.len() > 1
        && (t.starts_with('+') || t.starts_with('-'))
        && !t[1..].starts_with(' ')
        && !t.starts_with("--")
    {
        return 1;
    }
    // Terminal capture ("$ cargo test").
    if t.starts_with("$ ") {
        return 1;
    }
    // Very long unwrapped line — machine text doesn't wrap.
    if t.len() > 180 {
        return 1;
    }
    // Stopword-free multi-token line: authored English virtually always
    // carries a function word; key:value dumps and identifier soup don't.
    let toks: Vec<&str> = t.split_whitespace().collect();
    if toks.len() >= 5 && !line_has_stopword(&lower) {
        return 1;
    }
    // Majority of tokens look like code (paths, idents, calls).
    if toks.len() >= 3 {
        let codey = toks.iter().filter(|k| token_is_code(k)).count();
        if codey * 10 >= toks.len() * 6 {
            return 1;
        }
    }
    // Symbol-dense line (braces, colons, equals — dump shrapnel).
    if t.len() >= 12 && symbol_density(t) > 0.22 {
        return 1;
    }
    0
}

/// Tiny function-word list for the natural-language-or-not signal (NLoN's
/// strongest single feature). A ≥5-token line with *zero* of these is almost
/// never authored English.
const LINE_STOPWORDS: &[&str] = &[
    "the", "a", "an", "and", "or", "but", "if", "then", "else", "for", "to",
    "of", "in", "on", "at", "is", "are", "was", "were", "be", "been", "it",
    "this", "that", "these", "those", "with", "as", "by", "from", "so", "we",
    "you", "i", "not", "no", "do", "does", "did", "don't", "should", "must",
    "can", "could", "will", "would", "when", "what", "how", "why", "which",
    "there", "their", "our", "your", "my", "me", "us", "them", "they", "he",
    "she", "his", "her", "its", "also", "than", "into", "over", "under",
    "after", "before", "while", "where", "all", "any", "some", "please",
];

/// Does the (lowercased) line contain at least one English function word?
fn line_has_stopword(lower: &str) -> bool {
    lower.split_whitespace().any(|tok| {
        let w: String = tok.chars().filter(|c| c.is_ascii_alphabetic() || *c == '\'').collect();
        LINE_STOPWORDS.contains(&w.as_str())
    })
}

/// Ratio of machine-punctuation characters (braces, colons, equals, …) to
/// line length. Prose punctuation and backticks are excluded so a normal
/// technical sentence stays low.
fn symbol_density(t: &str) -> f64 {
    let total = t.chars().count().max(1);
    let sym = t
        .chars()
        .filter(|c| {
            !c.is_alphanumeric()
                && !c.is_whitespace()
                && !matches!(c, '.' | ',' | '!' | '?' | '\'' | '"' | '-' | '`')
        })
        .count();
    sym as f64 / total as f64
}

/// Numbered stack frame: "  3: core::ops::…", "0: rust_begin_unwind",
/// "#2 0x00007f8e…".
fn is_frame_line(t: &str) -> bool {
    if let Some(rest) = t.strip_prefix('#') {
        let digits = rest.chars().take_while(|c| c.is_ascii_digit()).count();
        return digits > 0 && rest[digits..].trim_start().starts_with("0x");
    }
    let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits == 0 {
        return false;
    }
    if let Some(r) = t[digits..].strip_prefix(": ") {
        return r.contains("::")
            || r.contains('/')
            || r.starts_with("0x")
            || r.split_whitespace().next().map(is_code_like).unwrap_or(false);
    }
    false
}

/// Leading ISO date (`2026-07-03…`) or clock time (`07:12:33…`), optionally
/// bracketed — the log-shipper heuristic for "this is a log line".
fn starts_with_timestamp(t: &str) -> bool {
    let s = t.trim_start_matches('[');
    let c: Vec<char> = s.chars().take(10).collect();
    if c.len() >= 10
        && c[..4].iter().all(|c| c.is_ascii_digit())
        && c[4] == '-'
        && c[5..7].iter().all(|c| c.is_ascii_digit())
        && c[7] == '-'
        && c[8..10].iter().all(|c| c.is_ascii_digit())
    {
        return true;
    }
    c.len() >= 8
        && c[0].is_ascii_digit()
        && c[1].is_ascii_digit()
        && c[2] == ':'
        && c[3].is_ascii_digit()
        && c[4].is_ascii_digit()
        && c[5] == ':'
        && c[6].is_ascii_digit()
        && c[7].is_ascii_digit()
}

/// First token is an all-caps log-level keyword (optionally bracketed).
fn starts_with_log_level(t: &str) -> bool {
    let first = t.split_whitespace().next().unwrap_or("");
    let w = first.trim_matches(|c: char| matches!(c, '[' | ']' | '(' | ')' | ':'));
    matches!(
        w,
        "INFO" | "WARN" | "WARNING" | "ERROR" | "DEBUG" | "TRACE" | "FATAL" | "PANIC"
            | "FAIL" | "PASS" // jest/tap runner result lines
    )
}

/// One of the first tokens is an exception headline ("TypeError:",
/// "java.lang.NullPointerException: …").
fn is_exception_headline(t: &str) -> bool {
    t.split_whitespace()
        .take(3)
        .any(|tok| tok.ends_with("Exception:") || tok.ends_with("Error:") || (tok.ends_with("Exception") && tok.contains('.')))
}

// ── Tokenisation & counting primitives ───────────────────────────────────────

/// Replace code-ish spans with a neutral `code` token so prose metrics (syllable
/// counts, sentence splits, readability) aren't corrupted by paths, identifiers,
/// `func()` calls, URLs, and fenced/inline code. Returns a prose-only copy.
fn mask_code(text: &str) -> String {
    let mut out = String::with_capacity(text.len());

    // (1) Fenced code blocks ```…``` → drop entirely.
    let without_fences = strip_fenced(text);

    // (2) Walk whitespace tokens; replace inline-`backtick`, URLs, paths, and
    //     identifier-ish tokens with "code".
    for (i, tok) in without_fences.split_whitespace().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        if token_is_code(tok) {
            // Preserve trailing sentence punctuation so sentence counting still
            // sees boundaries (".", "?", "!").
            let trailing = tok
                .chars()
                .rev()
                .take_while(|c| matches!(c, '.' | '!' | '?' | ':' | ';' | ','))
                .collect::<String>();
            out.push_str("code");
            // re-emit the trailing punctuation in original order
            for c in trailing.chars().rev() {
                out.push(c);
            }
        } else {
            out.push_str(tok);
        }
    }
    out
}

/// Remove ```…``` fenced blocks (and stray inline backtick spans collapse to a
/// single `code` token in [`token_is_code`]).
fn strip_fenced(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_fence = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            out.push_str(" code "); // keep a token where the block was
            continue;
        }
        if !in_fence {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Heuristic: does this whitespace token look like code (and so should be masked
/// out of prose)? URLs, file paths, `func()` calls, inline-backtick spans,
/// snake_case / CamelCase / dotted identifiers.
fn token_is_code(tok: &str) -> bool {
    let t = tok.trim_matches(|c: char| matches!(c, '.' | ',' | ':' | ';' | '"' | '\'' | '(' | ')'));
    if t.is_empty() {
        return false;
    }
    if t.starts_with('`') || t.ends_with('`') {
        return true;
    }
    if t.starts_with("http://") || t.starts_with("https://") {
        return true;
    }
    is_code_like(t)
}

/// Split into lowercased word tokens (alphanumerics plus inner `_`/`'`). Used
/// for length, syllable, and diversity measures over the **masked prose**.
fn tokenize_words(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in text.chars() {
        if ch.is_alphanumeric() || ch == '_' || ch == '\'' {
            cur.push(ch.to_ascii_lowercase());
        } else if !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Count sentence-ish units. Splits on `.`/`!`/`?`/`;`, the Japanese
/// terminators `。`/`！`/`？`, and newlines (each bulleted line counts as a
/// clause). Consecutive terminators collapse.
fn count_sentences(text: &str) -> usize {
    let mut count = 0usize;
    let mut in_sentence = false;
    for ch in text.chars() {
        if matches!(ch, '.' | '!' | '?' | '\n' | ';' | '。' | '！' | '？') {
            if in_sentence {
                count += 1;
                in_sentence = false;
            }
        } else if !ch.is_whitespace() {
            in_sentence = true;
        }
    }
    if in_sentence {
        count += 1;
    }
    count
}

/// Greg-Fast-style heuristic English syllable counter: count vowel groups, drop
/// a silent trailing `e`, floor at 1. Good enough for the aggregate ratios the
/// readability indices need.
fn count_syllables(word: &str) -> usize {
    let w: String = word.chars().filter(|c| c.is_ascii_alphabetic()).collect();
    if w.is_empty() {
        return usize::from(word.chars().any(|c| c.is_alphanumeric()));
    }
    let w = w.to_ascii_lowercase();
    let is_vowel = |c: char| matches!(c, 'a' | 'e' | 'i' | 'o' | 'u' | 'y');
    let mut count = 0usize;
    let mut prev_vowel = false;
    for c in w.chars() {
        let v = is_vowel(c);
        if v && !prev_vowel {
            count += 1;
        }
        prev_vowel = v;
    }
    // Silent trailing 'e' drops a beat — but not for a syllabic consonant+"le"
    // ending ("apple", "table"), where the "-le" carries its own syllable.
    let ends_cons_le = w.ends_with("le")
        && w.chars().rev().nth(2).map(|c| !is_vowel(c)).unwrap_or(false);
    if w.ends_with('e') && count > 1 && !ends_cons_le {
        count -= 1;
    }
    count.max(1)
}

/// Count code-like references in the *raw* text (occurrences, not distinct —
/// repeating a path across a multi-step prompt is fine; diversity guards spam).
fn count_code_refs(text: &str) -> usize {
    let mut count = text.matches('`').count() / 2; // backtick spans
    for tok in text.split_whitespace() {
        let t = tok.trim_matches(|c: char| {
            matches!(c, '.' | ',' | ':' | ';' | '"' | '\'' | '(' | ')' | '`')
        });
        if !t.is_empty() && is_code_like(t) {
            count += 1;
        }
    }
    count
}

/// True if a token looks like a code reference: path, `func()` call, file with
/// extension, snake_case, or CamelCase identifier.
fn is_code_like(t: &str) -> bool {
    if !t.chars().any(|c| c.is_alphabetic()) {
        return false;
    }
    if t.contains('/') && t.len() > 2 {
        return true; // path
    }
    if t.contains("::") {
        return true; // Rust/C++ qualified path
    }
    if let Some(idx) = t.find('(') {
        if idx > 0 && t[..idx].chars().all(|c| c.is_alphanumeric() || c == '_') {
            return true; // func( or func()
        }
    }
    if let Some(dot) = t.rfind('.') {
        let ext = &t[dot + 1..];
        if (1..=5).contains(&ext.len())
            && ext.chars().all(|c| c.is_ascii_alphanumeric())
            && dot > 0
            && t[..dot].chars().any(|c| c.is_alphabetic())
            && !t[..dot].contains(' ')
        {
            return true; // foo.rs, mod.py
        }
    }
    if t.contains('_') && t.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return true; // snake_case
    }
    let chars: Vec<char> = t.chars().collect();
    for i in 1..chars.len() {
        if chars[i].is_ascii_uppercase() && chars[i - 1].is_ascii_lowercase() {
            return true; // CamelCase / mixedCase
        }
    }
    false
}

/// Count structural cues: (bullets, numbered-steps, headings, code-fences).
fn count_structure(text: &str) -> (usize, usize, usize, usize) {
    let mut bullets = 0;
    let mut numbered = 0;
    let mut headings = 0;
    let mut code_fences = 0;
    for line in text.lines() {
        let l = line.trim_start();
        if l.is_empty() {
            continue;
        }
        if l.starts_with("```") {
            code_fences += 1;
            continue;
        }
        if l.starts_with('#') {
            headings += 1;
            continue;
        }
        let mut chars = l.chars();
        match chars.next() {
            Some('-') | Some('*') | Some('•') | Some('+')
                if l.chars().nth(1).map(|c| c.is_whitespace()).unwrap_or(false) =>
            {
                bullets += 1;
            }
            Some(c) if c.is_ascii_digit() => {
                let head: String = l.chars().take(4).collect();
                if head.contains('.') || head.contains(')') {
                    numbered += 1;
                }
            }
            _ => {}
        }
    }
    // each ``` toggles a fence; pairs => blocks
    (bullets, numbered, headings, code_fences / 2)
}

/// True for a character that is part of a word token (alphanumeric, `_`, `'`).
/// The single source of truth for the word-boundary split used by both
/// [`word_counts`] and historically by `lexicon_hits`.
#[inline]
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '\''
}

/// Occurrence count of every word token in `text`, split on word boundaries.
/// Built once per prompt and reused by all single-word lexicon lookups.
fn word_counts(text: &str) -> HashMap<&str, usize> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for tok in text.split(|c: char| !is_word_char(c)) {
        if !tok.is_empty() {
            *counts.entry(tok).or_insert(0) += 1;
        }
    }
    counts
}

/// Count lexicon hits. Single-word entries count every matching prose token (via
/// the precomputed `counts` map, gated by `word_set` so code-masked spans don't
/// score); multi-word entries count substring occurrences in the lowercased raw
/// text. `counts` must be [`word_counts`] over the same `lower` string.
fn lexicon_hits(
    lower: &str,
    counts: &HashMap<&str, usize>,
    word_set: &HashSet<&str>,
    lex: &[&str],
) -> usize {
    let mut hits = 0;
    for &entry in lex {
        if entry.contains(' ') {
            hits += lower.matches(entry).count();
        } else if word_set.contains(entry) {
            hits += counts.get(entry).copied().unwrap_or(0);
        }
    }
    hits
}

// ── Diversity (MATTR) ────────────────────────────────────────────────────────

/// Lexical diversity normalised to `0.0..=1.0`. Adaptive MATTR: the window is
/// `clamp(words/2, 10, 40)`. For prompts shorter than the window we fall back to
/// the whole-text type-token ratio, pulled toward a neutral 0.5 by a confidence
/// factor (a 5-word prompt has no room to show diversity, so it shouldn't be
/// unfairly rewarded *or* punished).
fn lexical_diversity(words: &[String]) -> f64 {
    let n = words.len();
    if n == 0 {
        return 0.0;
    }
    let window = (n / 2).clamp(10, 40);
    if n <= window {
        let ttr = type_token_ratio(words);
        let conf = (n as f64 / 12.0).min(1.0);
        return 0.5 * (1.0 - conf) + ttr * conf;
    }
    mattr(words, window)
}

fn type_token_ratio(words: &[String]) -> f64 {
    if words.is_empty() {
        return 0.0;
    }
    let types: HashSet<&str> = words.iter().map(|w| w.as_str()).collect();
    types.len() as f64 / words.len() as f64
}

/// Moving-Average Type-Token Ratio. Slides a fixed `window` one token at a time;
/// each window's TTR divides by the fixed window size. Average over all windows.
///
/// Single-pass O(n): a running multiset of the window's tokens is updated by one
/// removal + one insertion per step (the distinct-type count is the map size),
/// instead of rebuilding a `HashSet` over the whole slice at every position.
fn mattr(words: &[String], window: usize) -> f64 {
    let n = words.len();
    if window == 0 || n <= window {
        return type_token_ratio(words);
    }
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for w in &words[..window] {
        *counts.entry(w.as_str()).or_insert(0) += 1;
    }
    let win = window as f64;
    let mut sum = counts.len() as f64 / win;
    let mut steps = 1usize;
    for start in 1..=(n - window) {
        // token leaving the window on the left
        let out = words[start - 1].as_str();
        if let Some(c) = counts.get_mut(out) {
            *c -= 1;
            if *c == 0 {
                counts.remove(out);
            }
        }
        // token entering on the right
        *counts.entry(words[start + window - 1].as_str()).or_insert(0) += 1;
        sum += counts.len() as f64 / win;
        steps += 1;
    }
    sum / steps as f64
}

/// Measure of Textual Lexical Diversity (forward+backward, 0.72 threshold).
/// Exposed for external callers / reporting; the composite uses [`mattr`]
/// because it is stable on short prompt-length text.
pub fn mtld(words: &[String], threshold: f64) -> f64 {
    if words.is_empty() {
        return 0.0;
    }
    let f = mtld_pass(words.iter(), threshold);
    let b = mtld_pass(words.iter().rev(), threshold);
    (f + b) / 2.0
}

fn mtld_pass<'a, I>(iter: I, threshold: f64) -> f64
where
    I: Iterator<Item = &'a String>,
{
    let mut factors = 0.0f64;
    let mut types: HashSet<&str> = HashSet::new();
    let mut tokens = 0usize;
    let mut total = 0usize;
    let mut last_ttr = 1.0;
    for w in iter {
        total += 1;
        tokens += 1;
        types.insert(w.as_str());
        last_ttr = types.len() as f64 / tokens as f64;
        if last_ttr <= threshold {
            factors += 1.0;
            types.clear();
            tokens = 0;
            last_ttr = 1.0;
        }
    }
    if tokens > 0 {
        let denom = 1.0 - threshold;
        if denom > 0.0 {
            factors += (1.0 - last_ttr) / denom;
        }
    }
    if factors <= 0.0 {
        return total as f64;
    }
    total as f64 / factors
}

/// Repetition penalty in `0.6..=1.0`: the fraction of bigrams that are exact
/// repeats drives a multiplier on the composite, defeating phrase-farming like
/// "must test format must test format".
fn repetition_factor(words: &[String]) -> f64 {
    if words.len() < 4 {
        return 1.0;
    }
    let mut seen: HashSet<(&str, &str)> = HashSet::new();
    let mut repeats = 0usize;
    let mut total = 0usize;
    for pair in words.windows(2) {
        let bg = (pair[0].as_str(), pair[1].as_str());
        total += 1;
        if !seen.insert(bg) {
            repeats += 1;
        }
    }
    if total == 0 {
        return 1.0;
    }
    let ratio = repeats as f64 / total as f64;
    (1.0 - ratio).clamp(0.6, 1.0)
}

// ── Shaping functions ────────────────────────────────────────────────────────

/// Linear ratio of `count` to a cap, clamped to `0.0..=1.0`. The per-category
/// keyword cap that stops any one lexicon from farming a signal.
fn cap_ratio(count: usize, cap: usize) -> f64 {
    if cap == 0 {
        return 0.0;
    }
    (count as f64 / cap as f64).min(1.0)
}

/// Length-adequacy curve on prose word count. ~0 below 5 words, ramps to 1.0 by
/// ~40 words, stays at 1.0 through ~700, then tapers gently for rambling walls.
fn length_adequacy(words: usize) -> f64 {
    let n = words as f64;
    if n < 5.0 {
        return (n / 40.0).min(0.1);
    }
    if n <= 40.0 {
        let t = (n - 5.0) / 35.0; // 5..40 → 0..1
        return 1.0 - (1.0 - t) * (1.0 - t); // ease-out
    }
    if n <= 700.0 {
        return 1.0;
    }
    // taper 700..1400 → 1.0..0.6
    (1.0 - (n - 700.0) / 700.0 * 0.4).clamp(0.6, 1.0)
}

/// Japanese clarity proxy in `0.0..=1.0`. Readability indices don't transfer to
/// Japanese, so we use the dominant driver — sentence length. Full credit for
/// sentences of ~6–40 word-equivalents (≈12–80 characters), tapering for a
/// choppy stream of fragments or a tangled run-on. Neutral 0.6 for very short
/// prompts where the estimate is too noisy to trust.
fn ja_clarity(words_per_sentence: f64, words: usize) -> f64 {
    if words < 6 {
        return 0.6;
    }
    trapezoid(words_per_sentence, 2.0, 6.0, 40.0, 80.0)
}

/// Map readability to a clarity score in `0.0..=1.0` via a trapezoid band — full
/// credit for clear technical English (FK grade ~7–13 / Flesch ~35–85), tapering
/// for both a childishly-simple ask and a tangled run-on. Neutral 0.6 for very
/// short prompts where the estimate is too noisy to trust.
fn clarity_band(fk_grade: f64, flesch: f64, words: usize) -> f64 {
    if words < 8 {
        return 0.6;
    }
    let grade_fit = trapezoid(fk_grade, 4.0, 7.0, 13.0, 18.0);
    let flesch_fit = trapezoid(flesch, 20.0, 35.0, 85.0, 100.0);
    ((grade_fit + flesch_fit) / 2.0).clamp(0.0, 1.0)
}

/// Trapezoid membership: 0 outside `[min,max]`, 1 on the `[ideal_min,ideal_max]`
/// plateau, linear on the shoulders.
fn trapezoid(v: f64, min: f64, ideal_min: f64, ideal_max: f64, max: f64) -> f64 {
    if v <= min || v >= max {
        0.0
    } else if v >= ideal_min && v <= ideal_max {
        1.0
    } else if v < ideal_min {
        (v - min) / (ideal_min - min)
    } else {
        (max - v) / (max - ideal_max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore]
    fn diag_calibration_ja() {
        let cases = [
            ("vague", "いい感じにしておいて"),
            ("fix-vague", "バグを修正して"),
            ("tactical", "src/a.rs の `foo()` と src/b.rs の `bar()` を修正してください。JSON を返すこと。テストを追加し `cargo test` を実行してください。シグネチャは変更しないでください。この2つのファイルのみ変更すること。"),
            ("rich", "src/util.rs の `parse_range()` をリファクタして、上限が包含的なときのオフバイワンを修正してください。空の範囲のユニットテストを追加し、既存のテストが通ることを確認してください。公開シグネチャは変更しないでください。"),
            ("loaded", "背景: ネストしたリストのパーサが子要素を誤って数えています。目的として src/parser.rs の `parse_nested()` を修正し、末尾の区切り文字が余分な子要素を生成しないようにしてください。末尾区切りのケースの回帰テストを追加し、公開シグネチャは維持してください。`cargo test parser::` が通ることを確認。"),
            ("prohib", "公開シグネチャは変更しないでください。src/legacy.rs は触らないでください。新しい依存は禁止です。"),
        ];
        for (name, p) in cases {
            let s = score_prompt(p);
            let b = &s.breakdown;
            println!(
                "ja/{name:10} score={:5.1} {:11} words={} obj={:.2} grd={:.2} dir={:.2} ctx={:.2} ex={:.2} clar={:.2}",
                s.score, s.level.label(), s.words, b.objective, b.grounding, b.direction, b.context, b.examples, b.clarity
            );
        }
    }

    #[test]
    #[ignore]
    fn diag_calibration() {
        let cases = [
            ("vague-make", "make it better"),
            ("vague-fix", "fix the bug please"),
            ("tactical", "Edit `foo()` in src/a.rs and `bar()` in src/b.rs. Must return JSON. Add tests and run `cargo test`. Do not change signatures. Only touch those two files."),
            ("rich", "Refactor `parse_range()` in src/util.rs so it handles the off-by-one when the upper bound is inclusive. Add a unit test for the empty-range case and make sure the existing tests still pass. Do not change the public signature."),
            ("loaded", "The nested-list parser miscounts children. Fix `parse_nested()` in src/parser.rs so a trailing separator does not produce a phantom child. Add a regression test covering the trailing-separator case and keep the public signature unchanged. Done when `cargo test parser::` passes."),
            ("stuffed", "must must should ensure test test verify return format handle error case must test format must test format edge case must verify only"),
            ("prohib", "Do not change the public signature. Never touch src/legacy.rs. Avoid adding new dependencies. Don't reformat unrelated code."),
        ];
        for (name, p) in cases {
            let s = score_prompt(p);
            let b = &s.breakdown;
            println!(
                "{name:10} score={:5.1} {:11} obj={:.2} grd={:.2} dir={:.2} ctx={:.2} ex={:.2} core={:.2}",
                s.score, s.level.label(), b.objective, b.grounding, b.direction, b.context, b.examples, b.core()
            );
        }
    }

    #[test]
    fn enrichment_weights_sum_to_one() {
        assert!(
            (ENRICHMENT.sum() - 1.0).abs() < 1e-9,
            "enrichment weights sum = {}",
            ENRICHMENT.sum()
        );
    }

    #[test]
    fn syllables_basic() {
        assert_eq!(count_syllables("cat"), 1);
        assert_eq!(count_syllables("apple"), 2);
        assert_eq!(count_syllables("code"), 1);
        assert!(count_syllables("readability") >= 4);
    }

    #[test]
    fn code_detection_and_masking() {
        assert!(is_code_like("src/util.rs"));
        assert!(is_code_like("parse_range()"));
        assert!(is_code_like("fooBar"));
        assert!(!is_code_like("function"));
        let masked = mask_code("Fix `parse_range()` in src/util.rs now.");
        // paths/idents masked, prose words kept
        assert!(masked.contains("Fix"));
        assert!(masked.contains("code"));
        assert!(!masked.contains("parse_range"));
        assert!(!masked.contains("util.rs"));
    }

    #[test]
    fn vague_prompt_is_nascent() {
        let s = score_prompt("make it better");
        assert!(s.score < 25.0, "got {}", s.score);
        assert_eq!(s.level, MaturityLevel::Nascent);
    }

    #[test]
    fn rich_prompt_beats_vague_by_wide_margin() {
        let vague = score_prompt("fix the bug please");
        let rich = score_prompt(
            "Refactor `parse_range()` in src/util.rs so it handles the off-by-one \
             when the upper bound is inclusive. Add a unit test for the empty-range \
             case and make sure the existing tests still pass. Do not change the \
             public signature.",
        );
        assert!(
            rich.score > vague.score + 30.0,
            "rich {} vs vague {}",
            rich.score,
            vague.score
        );
        assert!(rich.breakdown.objective > 0.4);
        assert!(rich.breakdown.grounding > 0.4);
        assert!(rich.breakdown.direction > 0.3);
        assert!(rich.level >= MaturityLevel::Proficient);
    }

    /// Build a breakdown with the three multiplicative core slots set and
    /// everything else neutral — lets the core policy be tested in isolation.
    fn bd_core(objective: f64, grounding: f64, direction: f64) -> PromptScoreBreakdown {
        PromptScoreBreakdown {
            objective,
            grounding,
            direction,
            ..PromptScoreBreakdown::zero()
        }
    }

    #[test]
    fn core_is_multiplicative_with_floor() {
        // Geometric mean of three floored factors, ranging [CORE_MIN, 1]. A full
        // core → 1.0; an all-empty one → CORE_MIN.
        assert!((bd_core(1.0, 1.0, 1.0).core() - 1.0).abs() < 1e-9);
        let empty = bd_core(0.0, 0.0, 0.0).core();
        assert!((empty - CORE_MIN).abs() < 1e-9, "empty core {}", empty);
        // A single weak slot caps the core below the two-strong-slots case.
        assert!(bd_core(1.0, 1.0, 0.1).core() < bd_core(1.0, 1.0, 0.9).core());
    }

    #[test]
    fn missing_core_slot_caps_the_score() {
        // No direction (unbounded) → score capped well under "advanced" no
        // matter how concrete — this replaces the old hard control gate.
        let s = score_prompt(
            "Refactor `parse_range()` in src/util.rs and rename the helper in \
             src/helpers.rs to match the new module path.",
        );
        assert!(s.breakdown.direction < 0.35, "direction {}", s.breakdown.direction);
        assert!(s.score < 75.0, "unbounded prompt reached advanced: {}", s.score);
    }

    #[test]
    fn tactical_prompt_survives_missing_context() {
        // Concrete + bounded but no "why": context is *enrichment*, not core,
        // so the multiplicative core keeps this a proficient tactical ask — the
        // old tactical-exemption behaviour, now with no special case.
        let s = score_prompt(
            "Edit `foo()` in src/a.rs and `bar()` in src/b.rs. Must return JSON. \
             Add tests and run `cargo test`. Do not change signatures. Only touch \
             those two files.",
        );
        assert!(s.breakdown.context < 0.35, "should read as context-free");
        assert!(
            s.breakdown.objective >= 0.5 && s.breakdown.direction >= 0.4,
            "should be concrete+bounded (obj {:.2}, dir {:.2})",
            s.breakdown.objective,
            s.breakdown.direction,
        );
        assert!(s.level >= MaturityLevel::Proficient, "tactical prompt scored {}", s.score);
    }

    #[test]
    fn prohibitions_without_objective_flag_and_cap() {
        // All "don't", no "do": low objective drags the core, and the smell is
        // surfaced as a flag.
        let s = score_prompt(
            "Do not change the public signature. Never touch src/legacy.rs. Avoid \
             adding new dependencies. Don't reformat unrelated code.",
        );
        assert!(s.breakdown.objective < 0.2, "objective {}", s.breakdown.objective);
        assert!(s.flags.contains(&Flag::NoObjective), "flags {:?}", s.flags);
        assert!(s.score < 50.0, "prohibitions-only scored {}", s.score);
    }

    #[test]
    fn keyword_stuffing_is_capped() {
        let spam = score_prompt(
            "must must should ensure test test verify return format handle error \
             case must test format must test format edge case must verify only",
        );
        // No grounding (no code refs) → core capped; repetition penalty compounds.
        assert!(spam.score <= 69.0, "stuffed prompt scored {}", spam.score);
        assert!(spam.breakdown.grounding < 0.35, "grounding {}", spam.breakdown.grounding);
    }

    #[test]
    fn executable_acceptance_beats_bare_keyword() {
        // A named runnable check is stronger direction than the word "ensure".
        let runnable = score_prompt(
            "Add a retry wrapper in `src/net.rs`. Done when `cargo test net::` passes.",
        );
        let bare = score_prompt(
            "Add a retry wrapper in `src/net.rs`. Ensure it works and is correct.",
        );
        assert!(
            runnable.breakdown.direction > bare.breakdown.direction,
            "runnable {} vs bare {}",
            runnable.breakdown.direction,
            bare.breakdown.direction
        );
    }

    #[test]
    fn examples_slot_detected() {
        let with_ex = score_prompt(
            "Add a `slugify()` helper in `src/util.rs`. For example, slugify(\"Hi There\") \
             -> \"hi-there\" and slugify(\"a  b\") -> \"a-b\". Add tests.",
        );
        assert!(with_ex.breakdown.examples > 0.3, "examples {}", with_ex.breakdown.examples);
    }

    #[test]
    fn structured_multistep_scores_structure() {
        let s = score_prompt(
            "Implement the feature in three steps:\n\
             1. Add the `Config` struct to src/config.rs\n\
             2. Wire it into `main()` so the flag is parsed\n\
             3. Add a test in tests/config_test.rs covering the default value",
        );
        assert!(s.breakdown.structure > 0.5, "structure {}", s.breakdown.structure);
    }

    #[test]
    fn imprecise_verbs_do_not_earn_objective_credit() {
        // Paska's "not precise verb" smell: "handle"/"process"/"support" look
        // actionable but name no action — they must not score like a real verb.
        let precise = score_prompt(
            "Add pagination to `list_items()` in src/api.rs, capping the page size at 100.",
        );
        let imprecise = score_prompt(
            "Handle the errors and process the data and support pagination properly.",
        );
        assert!(
            precise.breakdown.objective > imprecise.breakdown.objective + 0.3,
            "precise {} vs imprecise {}",
            precise.breakdown.objective,
            imprecise.breakdown.objective
        );
        // The imprecise-only ask reads as objectiveless and scores low.
        assert!(imprecise.breakdown.objective < 0.2, "obj {}", imprecise.breakdown.objective);
        assert!(imprecise.score < 30.0, "imprecise-verb prompt scored {}", imprecise.score);
    }

    #[test]
    fn weak_words_lower_objective_and_grounding() {
        let crisp = score_prompt(
            "Add a `retry()` wrapper around the HTTP call in src/net.rs with a 3x cap.",
        );
        let weak = score_prompt(
            "Make the thing handle stuff appropriately and maybe make it better somehow.",
        );
        assert!(crisp.breakdown.grounding > weak.breakdown.grounding);
        assert!(crisp.breakdown.objective > weak.breakdown.objective);
    }

    #[test]
    fn abstains_on_no_authored_request() {
        // A pasted error line with no authored ask around it → no authored
        // words after segmentation → unscored, not a confident bad number.
        let s = score_prompt("error[E0308]: mismatched types --> src/a.rs:1:5");
        assert!(s.is_unscored(), "expected unscored, got {:?}", s.unscored);
        assert_eq!(s.score, 0.0);
        assert_eq!(s.unscored, Some("no authored request"));
    }

    #[test]
    fn abstains_on_unsupported_language() {
        // Japanese is supported (see the ja_* tests); a Latin-script language we
        // have no lexicon for (German here) is abstained rather than mis-scored.
        let s = score_prompt(
            "Implementieren Parser reparieren Funktion hinzufügen Tests Argumente \
             Rückgabewert Modul Klasse Schnittstelle",
        );
        assert_eq!(s.unscored, Some("unsupported language"));
    }

    // ── Japanese support ─────────────────────────────────────────────────────

    #[test]
    fn detect_lang_by_kana() {
        assert_eq!(detect_lang("バグを修正してください"), Lang::Japanese);
        assert_eq!(detect_lang("Fix the bug in src/util.rs"), Lang::English);
        // Code identifiers / kanji-free ASCII stay English.
        assert_eq!(detect_lang("refactor parse_range() in src/util.rs"), Lang::English);
        // A stray kana char or two is not enough to flip the language.
        assert_eq!(detect_lang("use the ア marker"), Lang::English);
    }

    #[test]
    fn tokenize_ja_splits_by_character_class() {
        // 実装(kanji) して(hira) ください(hira) → kanji run + hiragana run; the
        // Latin/punct is dropped. Katakana forms its own run.
        let toks = tokenize_ja("実装してテストする");
        assert!(toks.contains(&"実装".to_string()), "{toks:?}");
        assert!(toks.contains(&"テスト".to_string()), "{toks:?}");
        // Kana and kanji never merge into one token.
        assert!(toks.iter().all(|t| !t.chars().any(is_kanji) || t.chars().all(is_kanji)));
    }

    #[test]
    fn japanese_prompt_is_scored_not_abstained() {
        let s = score_prompt(
            "src/util.rs の `parse_range()` を修正して、空の範囲のテストを追加してください。",
        );
        assert!(!s.is_unscored(), "Japanese must be scored, got {:?}", s.unscored);
        assert_eq!(s.lang, Lang::Japanese);
        assert!(s.words > 0);
    }

    #[test]
    fn japanese_rich_beats_vague_by_wide_margin() {
        let vague = score_prompt("いい感じにしておいて");
        let rich = score_prompt(
            "src/util.rs の `parse_range()` をリファクタして、上限が包含的なときの\
             オフバイワンを修正してください。空の範囲のユニットテストを追加し、\
             既存のテストが通ることを確認してください。公開シグネチャは変更しないでください。",
        );
        assert!(
            rich.score > vague.score + 30.0,
            "rich {} vs vague {}",
            rich.score,
            vague.score
        );
        assert!(vague.level == MaturityLevel::Nascent, "vague was {}", vague.score);
        assert!(rich.level >= MaturityLevel::Proficient, "rich was {}", rich.score);
    }

    #[test]
    fn japanese_grounding_uses_language_agnostic_code_refs() {
        // Code paths / func() are ASCII and score grounding regardless of prose
        // language: the same ask with vs. without concrete refs differs.
        let grounded = score_prompt(
            "`parse_range()` を src/util.rs で修正し、`cargo test` を実行してください。",
        );
        let vague = score_prompt("パーサーをいい感じに直してください。");
        assert!(
            grounded.breakdown.grounding > vague.breakdown.grounding + 0.2,
            "grounded {} vs vague {}",
            grounded.breakdown.grounding,
            vague.breakdown.grounding
        );
    }

    #[test]
    fn japanese_weak_words_lower_objective() {
        // 適切に / ちゃんと (vague adverbs) drag the objective the way English
        // weak words do.
        let crisp = score_prompt("src/net.rs に3回上限のリトライ処理を実装してください。");
        let weak = score_prompt("その辺をいい感じに適切に何とかしておいてください。");
        assert!(crisp.breakdown.objective > weak.breakdown.objective);
    }

    #[test]
    fn english_short_prompt_is_scored_not_abstained() {
        // Narrow abstention: a short *English* ask stays scored (low, with a
        // flag), it is not refused.
        let s = score_prompt("fix the bug please");
        assert!(!s.is_unscored());
        assert!(s.flags.contains(&Flag::TooShort));
    }

    #[test]
    fn branch_shrinkage_pulls_small_samples_toward_prior() {
        // One strong prompt should not crown a branch: it is pulled toward the
        // neutral prior, so the branch score sits below the single-prompt score.
        let strong = "Refactor `parse_range()` in src/util.rs to fix the inclusive \
                      off-by-one. Add a regression test for the empty range and run \
                      `cargo test`. Do not change the public signature.";
        let one = score_prompt(strong).score;
        let branch = score_branch(vec![strong], 1);
        let expect = (BRANCH_PRIOR_STRENGTH * BRANCH_PRIOR_MEAN + 1.0 * one)
            / (BRANCH_PRIOR_STRENGTH + 1.0);
        assert!((branch.score - expect).abs() < 1e-6, "branch {} expect {}", branch.score, expect);
        // Pulled toward the prior: closer to it than the single prompt was.
        assert!(
            (branch.score - BRANCH_PRIOR_MEAN).abs() < (one - BRANCH_PRIOR_MEAN).abs(),
            "shrinkage should pull {} toward {} from {}",
            branch.score,
            BRANCH_PRIOR_MEAN,
            one
        );
    }

    #[test]
    fn branch_drops_unscored_prompts() {
        // An unsupported-language prompt alongside a real one is excluded from
        // the mean but still counts against coverage.
        let real = "Add a test for `foo()` in src/foo.rs and run `cargo test`.";
        let branch = score_branch(
            vec![real, "Implementieren Parser Funktion Argumente Rückgabewert Modul Klasse Schnittstelle"],
            2,
        );
        assert_eq!(branch.scored_prompts, 1);
        assert_eq!(branch.ai_commits, 2);
        assert!(branch.low_confidence);
    }

    #[test]
    fn branch_aggregation_is_length_weighted_and_shrunk() {
        let a = "fix it"; // short, low
        let b = "Add a unit test for `foo()` in src/foo.rs and ensure it passes \
                 without changing the public signature, covering the empty input case.";
        let branch = score_branch(vec![a, b], 2);
        assert_eq!(branch.scored_prompts, 2);
        assert_eq!(branch.ai_commits, 2);
        assert!((branch.coverage - 1.0).abs() < 1e-9);
        let sa = score_prompt(a);
        let sb = score_prompt(b);
        let wa = sa.words.clamp(20, 250) as f64;
        let wb = sb.words.clamp(20, 250) as f64;
        let lw = (sa.score * wa + sb.score * wb) / (wa + wb);
        let plain = (sa.score + sb.score) / 2.0;
        // Length weighting favors the longer, better prompt over a plain mean.
        assert!(lw > plain, "length weighting should lift {} above {}", lw, plain);
        // The branch score is that length-weighted mean, then shrunk toward the
        // prior (n = 2 prompts).
        let expect = (BRANCH_PRIOR_STRENGTH * BRANCH_PRIOR_MEAN + 2.0 * lw)
            / (BRANCH_PRIOR_STRENGTH + 2.0);
        assert!((branch.score - expect).abs() < 1e-6, "branch {} expect {}", branch.score, expect);
    }

    #[test]
    fn branch_coverage_low_confidence() {
        // 1 real prompt out of 4 ai commits → coverage 0.25 → low confidence
        let branch = score_branch(vec!["Add a test for `foo()` in src/foo.rs"], 4);
        assert_eq!(branch.scored_prompts, 1);
        assert_eq!(branch.ai_commits, 4);
        assert!(branch.low_confidence);
        assert!((branch.coverage - 0.25).abs() < 1e-9);
    }

    #[test]
    fn branch_empty_when_no_prompts() {
        assert!(score_branch(vec!["", "   "], 2).is_empty());
        assert!(score_branch(Vec::<String>::new(), 0).is_empty());
    }

    #[test]
    fn levels_partition_score_range() {
        assert_eq!(MaturityLevel::from_score(10.0), MaturityLevel::Nascent);
        assert_eq!(MaturityLevel::from_score(30.0), MaturityLevel::Developing);
        assert_eq!(MaturityLevel::from_score(60.0), MaturityLevel::Proficient);
        assert_eq!(MaturityLevel::from_score(80.0), MaturityLevel::Advanced);
        assert_eq!(MaturityLevel::from_score(95.0), MaturityLevel::Exemplary);
    }

    #[test]
    fn mattr_and_mtld_reward_variety() {
        let varied: Vec<String> =
            "the quick brown fox jumps over a lazy dog while seven owls watch from oaks"
                .split_whitespace().map(String::from).collect();
        let repet: Vec<String> =
            "spam spam spam spam spam spam spam spam spam spam spam spam spam spam"
                .split_whitespace().map(String::from).collect();
        assert!(mattr(&varied, 10) > mattr(&repet, 10));
        assert!(mtld(&varied, 0.72) > mtld(&repet, 0.72));
    }

    #[test]
    fn repetition_penalty_engages() {
        let toks: Vec<String> = "must test must test must test must test"
            .split_whitespace().map(String::from).collect();
        assert!(repetition_factor(&toks) < 1.0);
        let uniq: Vec<String> = "add a retry wrapper around the failing network call"
            .split_whitespace().map(String::from).collect();
        assert_eq!(repetition_factor(&uniq), 1.0);
    }

    #[test]
    fn clarity_band_penalizes_extremes() {
        assert_eq!(clarity_band(9.0, 60.0, 50), 1.0);
        assert!(clarity_band(2.0, 95.0, 50) < 1.0);
        assert!(clarity_band(22.0, 5.0, 50) < 1.0);
        assert_eq!(clarity_band(9.0, 60.0, 4), 0.6);
    }

    #[test]
    fn empty_and_flags() {
        // Whitespace-only → no authored request → abstained.
        let e = score_prompt("   ");
        assert_eq!(e.score, 0.0);
        assert!(e.is_unscored());
        assert!(e.flags.is_empty());
        // A real (if terse) English ask is scored, with a TooShort flag.
        let short = score_prompt("fix bug");
        assert!(!short.is_unscored());
        assert!(short.flags.contains(&Flag::TooShort));
        assert!(short.flags.len() <= 2);
    }

    #[test]
    fn readability_numbers_populated() {
        let s = score_prompt(
            "Refactor the parser so that it correctly handles nested quotes without \
             breaking the existing escape sequences in the regression suite.",
        );
        assert!(s.breakdown.flesch_reading_ease != 0.0);
        assert!(s.breakdown.fk_grade != 0.0);
        assert!(s.breakdown.gunning_fog > 0.0);
    }

    // ── Optimization equivalence guards ──────────────────────────────────────
    //
    // The fast paths (sliding-window MATTR, precomputed word-count map for
    // lexicon hits) must be bit-for-bit equivalent to the original naive
    // definitions they replaced. These tests reimplement the naive versions and
    // assert agreement across a spread of inputs, so a future "optimization"
    // that changes the *answer* can't pass.

    fn toks(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    /// The pre-optimization MATTR: rebuild a `HashSet` over each window slice.
    fn naive_mattr(words: &[String], window: usize) -> f64 {
        let n = words.len();
        if window == 0 || n <= window {
            return type_token_ratio(words);
        }
        let mut sum = 0.0;
        let mut count = 0usize;
        for start in 0..=(n - window) {
            let slice = &words[start..start + window];
            let types: HashSet<&str> = slice.iter().map(|w| w.as_str()).collect();
            sum += types.len() as f64 / window as f64;
            count += 1;
        }
        sum / count as f64
    }

    /// Deterministic pseudo-random token vector (LCG) over a small alphabet so we
    /// exercise repeats, runs, and turnover without a PRNG dependency.
    fn gen_tokens(seed: u64, len: usize, distinct: u64) -> Vec<String> {
        let mut state = seed.wrapping_add(0x9e3779b97f4a7c15);
        (0..len)
            .map(|_| {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                format!("w{}", (state >> 33) % distinct)
            })
            .collect()
    }

    #[test]
    fn mattr_fast_matches_naive() {
        // Hand-picked edge shapes.
        let cases: Vec<(Vec<String>, usize)> = vec![
            (toks("a a a a a a a a a a a a"), 4),
            (toks("a b c d e f g h i j k l"), 4),
            (toks("a b a b a b a b a b a b"), 5),
            (toks("the cat sat on the mat then the cat ran far"), 3),
            (toks("x y z"), 10),               // n <= window → TTR fallback
            (toks("one two three four five"), 5), // n == window
        ];
        for (words, window) in &cases {
            let fast = mattr(words, *window);
            let naive = naive_mattr(words, *window);
            assert!(
                (fast - naive).abs() < 1e-12,
                "mattr mismatch (window {window}): fast {fast} vs naive {naive} on {words:?}"
            );
        }
        // Fuzz a spread of lengths, windows, and distinct-token counts.
        for seed in 0..40u64 {
            let len = 1 + (seed as usize * 7) % 200;
            let distinct = 1 + (seed % 30);
            let window = 1 + (seed as usize * 3) % 45;
            let words = gen_tokens(seed, len, distinct);
            let fast = mattr(&words, window);
            let naive = naive_mattr(&words, window);
            assert!(
                (fast - naive).abs() < 1e-9,
                "fuzz mattr mismatch seed={seed} len={len} window={window}: {fast} vs {naive}"
            );
        }
    }

    /// The pre-optimization single-word count: re-split the whole text per probe.
    fn naive_word_count(text: &str, entry: &str) -> usize {
        text.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '\''))
            .filter(|t| *t == entry)
            .count()
    }

    #[test]
    fn word_counts_matches_naive_split() {
        let texts = [
            "must test must test, must verify only.",
            "src/test.rs holds test cases; the test runner runs tests.",
            "don't can't won't don't",
            "alpha_beta alpha beta ALPHA Alpha",
            "",
            "   ",
        ];
        for t in texts {
            let lower = t.to_ascii_lowercase();
            let counts = word_counts(&lower);
            // Every probe word — present or absent — must agree with the old scan.
            for probe in ["test", "must", "tests", "don't", "alpha", "alpha_beta", "zzz", "the"] {
                let fast = counts.get(probe).copied().unwrap_or(0);
                let naive = naive_word_count(&lower, probe);
                assert_eq!(fast, naive, "word_counts[{probe}] on {t:?}: {fast} != {naive}");
            }
        }
    }

    #[test]
    fn lexicon_hits_counts_repeats_and_multiword() {
        let lower = "we must test and must verify; do it as needed and as needed again.";
        let counts = word_counts(lower);
        let word_set: HashSet<&str> =
            lower.split(|c: char| !is_word_char(c)).filter(|t| !t.is_empty()).collect();
        // single-word entry counted per occurrence
        assert_eq!(lexicon_hits(lower, &counts, &word_set, &["must"]), 2);
        // multi-word entry counted by substring occurrence (independent of word_set)
        assert_eq!(lexicon_hits(lower, &counts, &word_set, &["as needed"]), 2);
        // mixed list sums both
        assert_eq!(lexicon_hits(lower, &counts, &word_set, &["must", "verify", "as needed"]), 5);
        // absent entry contributes nothing
        assert_eq!(lexicon_hits(lower, &counts, &word_set, &["refactor"]), 0);
    }

    #[test]
    fn lexicon_hits_gate_excludes_code_only_words() {
        // "test" appears only inside a code-ish path → masked out of prose, so it
        // is NOT in word_set and must not score, even though it is in the raw text.
        let f = Features::extract("Update src/test_helpers.rs to import the helper module.");
        // verification lexicon contains "test"; the only occurrence is in a path.
        assert_eq!(f.verification, 0, "code-path 'test' must not count as verification");
        // …whereas a prose 'test' does count.
        let g = Features::extract("Add a test and then verify the output module.");
        assert!(g.verification >= 2, "prose test+verify should score, got {}", g.verification);
    }

    #[test]
    fn syllable_and_polysyllable_single_pass_is_correct() {
        // The folded single pass must total syllables and count >=3 the same as
        // two independent walks would.
        let f = Features::extract("readability complexity matters for documentation clarity");
        let toks = tokenize_words("readability complexity matters for documentation clarity");
        let expect_syll: usize = toks.iter().map(|w| count_syllables(w)).sum();
        let expect_poly = toks.iter().filter(|w| count_syllables(w) >= 3).count();
        assert_eq!(f.syllables, expect_syll);
        assert_eq!(f.polysyllables, expect_poly);
        assert!(f.polysyllables >= 3, "several long words expected, got {}", f.polysyllables);
    }

    #[test]
    fn branch_weighted_mean_unaffected_by_precompute() {
        // Reorders/precomputes weights; the rolled-up score must equal the
        // hand-rolled length-weighted mean of the per-prompt scores.
        let prompts = vec![
            "fix it",
            "Add a unit test for `foo()` in src/foo.rs and ensure it passes \
             without changing the public signature, covering the empty input case.",
            "Refactor `parse()` in src/p.rs; must keep the signature; add a test.",
        ];
        let branch = score_branch(prompts.clone(), prompts.len());
        let scored: Vec<PromptScore> = prompts.iter().map(|p| score_prompt(p)).collect();
        let wsum: f64 = scored.iter().map(|s| s.words.clamp(20, 250) as f64).sum();
        let lw: f64 = scored
            .iter()
            .map(|s| s.score * s.words.clamp(20, 250) as f64)
            .sum::<f64>()
            / wsum;
        // Roll-up equals the length-weighted mean, then shrunk toward the prior.
        let n = scored.len() as f64;
        let expect = (BRANCH_PRIOR_STRENGTH * BRANCH_PRIOR_MEAN + n * lw) / (BRANCH_PRIOR_STRENGTH + n);
        assert!((branch.score - expect).abs() < 1e-9, "branch {} vs expect {}", branch.score, expect);
    }

    #[test]
    fn scoring_is_deterministic_and_order_independent() {
        let p = "Implement retry with backoff in `src/net.rs`:\n\
                 - cap at 3 attempts, only on 5xx\n\
                 - add a test for the give-up path\n\
                 Do not change the public signature.";
        // Same input → same score, every time (no map-iteration-order leakage).
        let a = score_prompt(p);
        let b = score_prompt(p);
        assert_eq!(a, b);
        // Branch roll-up independent of prompt order.
        let x = "Add a test for `foo()` in src/foo.rs; must pass.";
        let s1 = score_branch(vec![p, x], 2);
        let s2 = score_branch(vec![x, p], 2);
        assert!((s1.score - s2.score).abs() < 1e-9);
        assert_eq!(s1.level, s2.level);
    }

    // ── Artifact segmentation & evidence ─────────────────────────────────────

    #[test]
    fn segments_rust_panic_from_prose() {
        let text = "Fix the parser bug shown below.\n\
                    running 3 tests\n\
                    test parser::tests::nested ... FAILED\n\
                    thread 'parser::tests::nested' panicked at src/parser.rs:214:9:\n\
                    assertion `left == right` failed\n\
                    \u{20}\u{20}left: 3\n\
                    \u{20}\u{20}right: 2\n\
                    stack backtrace:\n\
                    \u{20}\u{20} 0: rust_begin_unwind\n\
                    \u{20}\u{20}           at /rustc/07dca48/library/std/src/panicking.rs:652:5\n\
                    Keep the public signature unchanged.";
        let seg = segment_artifacts(text);
        assert!(seg.authored.contains("Fix the parser bug"));
        assert!(seg.authored.contains("Keep the public signature"));
        assert!(!seg.authored.contains("panicked"), "authored: {}", seg.authored);
        assert!(!seg.authored.contains("rust_begin_unwind"));
        assert!(!seg.authored.contains("FAILED"));
        assert!(seg.artifact_lines >= 8);
        assert_eq!(seg.authored_lines, 2);
    }

    #[test]
    fn segments_python_traceback_java_frames_and_diffs() {
        let py = segment_artifacts(
            "The import fails, trace below.\n\
             Traceback (most recent call last):\n\
             \u{20}\u{20}File \"app.py\", line 3, in main\n\
             TypeError: run() missing 1 required argument: 'x'\n\
             Make main() pass a default.",
        );
        assert!(!py.authored.contains("Traceback"));
        assert!(!py.authored.contains("TypeError"));
        assert!(py.authored.contains("Make main() pass a default."));

        let java = segment_artifacts(
            "NPE on startup:\n\
             Exception in thread \"main\" java.lang.NullPointerException\n\
             \tat com.foo.Bar.baz(Bar.java:42)\n\
             Guard the config lookup.",
        );
        assert!(!java.authored.contains("NullPointerException"));
        assert!(!java.authored.contains("com.foo.Bar.baz"));
        assert!(java.authored.contains("Guard the config lookup."));

        let diff = segment_artifacts(
            "Apply this diff and add a test:\n\
             --- a/src/foo.rs\n\
             +++ b/src/foo.rs\n\
             @@ -1,3 +1,4 @@\n\
             +use std::fmt;\n\
             \u{20}fn main() {}\n",
        );
        assert!(!diff.authored.contains("@@"));
        assert!(!diff.authored.contains("+use"));
        assert!(diff.authored.contains("Apply this diff"));
        assert!(diff.artifact_lines >= 5);
    }

    #[test]
    fn structured_prose_is_not_misread_as_artifact() {
        // Bullets, numbered steps, backticked idents, paths — all authored.
        let seg = segment_artifacts(
            "Harden `Invoice::finalize()` in src/billing.rs in three steps:\n\
             - Reject a zero-quantity item by returning `Err(BillingError::EmptyLine)`.\n\
             1. Round each subtotal to two decimals.\n\
             2. Sanitize the note to drop control characters.\n\
             Keep the signature stable and edit only billing.rs.",
        );
        assert_eq!(seg.artifact_lines, 0, "authored: {}", seg.authored);
        assert_eq!(seg.authored_lines, 5);
    }

    #[test]
    fn fenced_interior_is_artifact_but_fence_structure_survives() {
        let text = "Reproduce with this snippet:\n```\nspam0 = 1\nspam1 = 2\nspam2 = 3\n```\nThen fix the overflow in `sum()`.";
        let seg = segment_artifacts(text);
        assert!(!seg.authored.contains("spam1"));
        assert!(seg.authored.contains("```"), "fence markers must stay for structure");
        assert_eq!(seg.artifact_lines, 3);
        // End-to-end: the code_fences structure signal still counts the block.
        let f = Features::extract(text);
        assert_eq!(f.code_fences, 1);
        assert!(f.artifact_lines == 3);
    }

    #[test]
    fn evidence_bonus_is_additive_and_framing_tops_it_up() {
        let ask = "Fix the flaky retry in src/net.rs and add a regression test.";
        let plain = score_prompt(ask);
        assert_eq!(plain.breakdown.evidence, 0.0);

        // Same ask + a pasted diagnostic → identical craft signals, evidence 0.7,
        // score up by exactly the bonus.
        let with_log = score_prompt(&format!("{ask}\nerror[E0308]: mismatched types"));
        assert!((with_log.breakdown.evidence - 0.7).abs() < 1e-9);
        assert!((with_log.score - plain.score - EVIDENCE_BONUS_MAX * 0.7).abs() < 1e-6);
        assert_eq!(with_log.words, plain.words, "paste must not count as words");

        // Framing the paste ("the error below") tops evidence up to 1.0.
        let framed = score_prompt(&format!(
            "{ask} The error below shows the failing case.\nerror[E0308]: mismatched types"
        ));
        assert!((framed.breakdown.evidence - 1.0).abs() < 1e-9);
    }

    #[test]
    fn mostly_paste_flag_fires_on_thin_ask_around_log_wall() {
        let mut p = String::from("fix this\n");
        for i in 0..12 {
            p.push_str(&format!("error[E0308]: mismatched types --> src/a.rs:{i}:5\n"));
        }
        let s = score_prompt(&p);
        assert!(s.flags.contains(&Flag::MostlyPaste), "flags: {:?}", s.flags);
        // Artifact-only paste: no authored request at all → abstained, not scored.
        let only = score_prompt("error[E0308]: mismatched types --> src/a.rs:1:5");
        assert_eq!(only.words, 0);
        assert!(only.is_unscored());
        assert_eq!(only.unscored, Some("no authored request"));
    }

    #[test]
    fn long_prompt_does_not_panic_and_scores_in_range() {
        // Exercises the sliding-window MATTR well past the window size.
        let big = "Refactor the module carefully and add tests. ".repeat(120);
        let s = score_prompt(&big);
        assert!((0.0..=100.0).contains(&s.score));
        assert!(s.words > 500);
    }
}
