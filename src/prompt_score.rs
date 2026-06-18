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
//! * **Readability ≠ maturity.** A terse, precise ask
//!   (*"fix the off-by-one in `parse_range()` in src/util.rs, add a test"*) is
//!   an *excellent* prompt that scores badly on raw reading-ease. So readability
//!   is used only as a **trapezoid band** (both extremes penalised), is one of
//!   the lowest-weighted signals, and is computed on a **code-masked** copy of
//!   the text so file paths and `func()` tokens don't corrupt the syllable
//!   counter.
//!
//! # The composite (locked with the `codex` agent over i5h)
//!
//! Seven sub-signals, each normalised to `0.0..=1.0`, combined by a fixed
//! weighted sum (see [`WEIGHTS`], which sums to 1.0):
//!
//! | Signal          | Weight | Captures                                          |
//! |-----------------|-------:|---------------------------------------------------|
//! | `specificity`   | 24%    | concreteness (code refs, idents) − vague words    |
//! | `control`       | 24%    | constraints, output shape, acceptance/verification|
//! | `context`       | 18%    | background, goal/why, current state, grounding    |
//! | `structure`     | 10%    | decomposition — bullets, steps, multi-sentence    |
//! | `diversity`     | 10%    | lexical richness (adaptive MATTR), non-repetitive |
//! | `clarity`       |  8%    | readability in a target band (trapezoid)          |
//! | `adequacy`      |  6%    | length in a sweet spot (not one word, not a wall) |
//!
//! On top of the weighted sum sit **anti-gaming guards** (locked with codex):
//! per-category keyword caps, a repetition penalty, hard length caps, and
//! *balance gates* — a keyword-stuffed but context-free prompt is capped at 69
//! so it can never read as "advanced".
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

// ── Weights ──────────────────────────────────────────────────────────────────

// TODO(empirical-calibration, v2): these weights — and the gate thresholds in
// `score_prompt` — are *normative* (hand-tuned, locked with codex). They are an
// explainable proxy, not a validated model. The `PromptScoreBreakdown` features
// are already exposed per prompt, so a future PR can join them against h5i's
// existing per-commit outcome signals — test pass/fail (`metadata::TestMetrics`),
// review-flag score (`ReviewPoint`), diff churn, and later reverts — over a real
// commit corpus and *learn* / validate these weights while keeping the features
// explainable. Do NOT regress from a tiny biased sample: that needs a corpus and
// confound controls (a senior writes good prompts AND good code). No durable
// schema change belongs here — feature persistence is its own design.

/// Relative weights of the seven sub-signals. Sums to 1.0 (asserted in tests).
/// Specificity and control dominate (they are what a manager actually wants to
/// see — "the engineer was concrete and bounded the agent"); the
/// readability-derived `clarity` band is the smallest voice because it is the
/// signal most likely to mislead on technical text.
pub const WEIGHTS: Weights = Weights {
    specificity: 0.24,
    control: 0.24,
    context: 0.18,
    structure: 0.10,
    diversity: 0.10,
    clarity: 0.08,
    adequacy: 0.06,
};

/// Weighting of the seven sub-signals. See [`WEIGHTS`].
#[derive(Debug, Clone, Copy)]
pub struct Weights {
    pub specificity: f64,
    pub control: f64,
    pub context: f64,
    pub structure: f64,
    pub diversity: f64,
    pub clarity: f64,
    pub adequacy: f64,
}

impl Weights {
    #[cfg(test)]
    fn sum(&self) -> f64 {
        self.specificity
            + self.control
            + self.context
            + self.structure
            + self.diversity
            + self.clarity
            + self.adequacy
    }
}

// ── Public result types ──────────────────────────────────────────────────────

/// The seven normalised sub-signals (`0.0..=1.0`) plus the raw readability
/// numbers, retained for transparent display.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PromptScoreBreakdown {
    pub specificity: f64,
    pub control: f64,
    pub context: f64,
    pub structure: f64,
    pub diversity: f64,
    pub clarity: f64,
    pub adequacy: f64,
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
            specificity: 0.0,
            control: 0.0,
            context: 0.0,
            structure: 0.0,
            diversity: 0.0,
            clarity: 0.0,
            adequacy: 0.0,
            flesch_reading_ease: 0.0,
            fk_grade: 0.0,
            gunning_fog: 0.0,
        }
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
        }
    }
}

/// Score for a single prompt.
#[derive(Debug, Clone, PartialEq)]
pub struct PromptScore {
    /// Composite maturity score, `0.0..=100.0` (after guards & caps).
    pub score: f64,
    pub level: MaturityLevel,
    pub breakdown: PromptScoreBreakdown,
    /// Prose word count (code-masked).
    pub words: usize,
    /// Up to two diagnostic flags, weakest dimension first.
    pub flags: Vec<Flag>,
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
    if f.words == 0 {
        return PromptScore {
            score: 0.0,
            level: MaturityLevel::Nascent,
            breakdown: PromptScoreBreakdown::zero(),
            words: 0,
            flags: vec![Flag::TooShort],
        };
    }

    let breakdown = f.breakdown();

    // Weighted sum → 0..100.
    let w = &WEIGHTS;
    let mut score = 100.0
        * (breakdown.specificity * w.specificity
            + breakdown.control * w.control
            + breakdown.context * w.context
            + breakdown.structure * w.structure
            + breakdown.diversity * w.diversity
            + breakdown.clarity * w.clarity
            + breakdown.adequacy * w.adequacy);

    // ── Anti-gaming guards ──────────────────────────────────────────────────
    // (1) Repetition penalty: a prompt that farms keywords by repeating phrases
    //     ("must test format must test format") is multiplied down.
    score *= f.repetition_factor;

    // (2) Balance gates — you cannot look mature on one axis alone.
    score = apply_balance_gates(score, &breakdown);

    // (3) Hard length caps — short prompts can't score high no matter how many
    //     keywords they pack.
    if f.words < 8 {
        score = score.min(20.0);
    } else if f.words < 15 {
        score = score.min(45.0);
    } else if f.words > 1200 && breakdown.structure < 0.6 {
        // a 1200+ word unstructured wall is a dump, not a mature prompt
        score = score.min(75.0);
    }

    let score = score.clamp(0.0, 100.0);

    PromptScore {
        level: MaturityLevel::from_score(score),
        flags: f.flags(&breakdown),
        words: f.words,
        breakdown,
        score,
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
/// by pooling vocabulary and structure.
pub fn score_branch<I, S>(prompts: I, ai_commits: usize) -> BranchPromptScore
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let scored: Vec<PromptScore> = prompts
        .into_iter()
        .filter(|p| !p.as_ref().trim().is_empty())
        .map(|p| score_prompt(p.as_ref()))
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
        specificity: wmean_b(&|b| b.specificity),
        control: wmean_b(&|b| b.control),
        context: wmean_b(&|b| b.context),
        structure: wmean_b(&|b| b.structure),
        diversity: wmean_b(&|b| b.diversity),
        clarity: wmean_b(&|b| b.clarity),
        adequacy: wmean_b(&|b| b.adequacy),
        flesch_reading_ease: wmean_b(&|b| b.flesch_reading_ease),
        fk_grade: wmean_b(&|b| b.fk_grade),
        gunning_fog: wmean_b(&|b| b.gunning_fog),
    };
    let score = wmean(&|s| s.score);

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
struct Features {
    /// Prose word count (code spans masked out before counting).
    words: usize,
    /// Prose tokens (lowercased), code spans removed — for diversity/readability.
    prose_tokens: Vec<String>,
    sentences: usize,
    syllables: usize,
    polysyllables: usize,
    // ── concreteness inputs ──
    code_refs: usize,
    quoted: usize,
    numbers: usize,
    action_verbs: usize,
    weak_words: usize,
    // ── context inputs ──
    context_markers: usize,
    grounding_refs: usize,
    // ── control inputs ──
    constraints: usize,
    output_shape: usize,
    verification: usize,
    edge_cases: usize,
    safety: usize,
    scope: usize,
    // ── structure inputs ──
    bullets: usize,
    numbered: usize,
    headings: usize,
    code_fences: usize,
    // ── anti-gaming ──
    repetition_factor: f64,
}

impl Features {
    fn extract(text: &str) -> Self {
        // Mask code/paths/URLs so prose metrics aren't corrupted, but keep the
        // raw text for code-ref counting and lexicon matching.
        let masked = mask_code(text);
        let prose_tokens = tokenize_words(&masked);
        let words = prose_tokens.len();
        // Single pass over the prose tokens: total syllables and the polysyllable
        // (>=3) count together, rather than walking the vec twice.
        let mut syllables = 0usize;
        let mut polysyllables = 0usize;
        for w in &prose_tokens {
            let s = count_syllables(w);
            syllables += s;
            if s >= 3 {
                polysyllables += 1;
            }
        }
        let sentences = count_sentences(&masked).max(1);
        let (bullets, numbered, headings, code_fences) = count_structure(text);

        let lower = text.to_ascii_lowercase();
        let word_set: HashSet<&str> = prose_tokens.iter().map(|s| s.as_str()).collect();
        // Word-occurrence map over the raw lowercased text, built once and shared
        // by every single-word lexicon lookup below. Replaces re-splitting the
        // whole text per lexicon entry (was O(entries × text_len)). Keyed on the
        // same word-boundary split the old per-entry filter used, so counts are
        // identical; `word_set` still gates so only words appearing as *prose*
        // (not code-masked spans) score.
        let lower_counts = word_counts(&lower);

        Features {
            code_refs: count_code_refs(text),
            quoted: text.matches('`').count() / 2 + text.matches('"').count() / 2,
            numbers: prose_tokens
                .iter()
                .filter(|w| w.chars().any(|c| c.is_ascii_digit()))
                .count(),
            action_verbs: lexicon_hits(&lower, &lower_counts, &word_set, ACTION_VERBS),
            weak_words: lexicon_hits(&lower, &lower_counts, &word_set, WEAK_WORDS),
            context_markers: lexicon_hits(&lower, &lower_counts, &word_set, CONTEXT_MARKERS),
            grounding_refs: lexicon_hits(&lower, &lower_counts, &word_set, GROUNDING_REFS),
            constraints: lexicon_hits(&lower, &lower_counts, &word_set, CONSTRAINTS),
            output_shape: lexicon_hits(&lower, &lower_counts, &word_set, OUTPUT_SHAPE),
            verification: lexicon_hits(&lower, &lower_counts, &word_set, VERIFICATION),
            edge_cases: lexicon_hits(&lower, &lower_counts, &word_set, EDGE_CASES),
            safety: lexicon_hits(&lower, &lower_counts, &word_set, SAFETY),
            scope: lexicon_hits(&lower, &lower_counts, &word_set, SCOPE),
            repetition_factor: repetition_factor(&prose_tokens),
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

        // ── Specificity = concreteness − vagueness ──────────────────────────
        // Concreteness from capped category contributions (no single category
        // can farm the whole signal).
        let concreteness = 0.40 * cap_ratio(self.code_refs, 4)
            + 0.22 * cap_ratio(self.action_verbs, 3)
            + 0.20 * cap_ratio(self.quoted, 3)
            + 0.18 * cap_ratio(self.numbers, 4);
        // Vagueness penalty from the NALABS/Femmer requirements-smell lexicon:
        // density of weak words (per 100 words) drags the signal down.
        let vagueness = (per100(self.weak_words) / 8.0).clamp(0.0, 1.0);
        let specificity = (concreteness - 0.5 * vagueness).clamp(0.0, 1.0);

        // ── Context grounding ───────────────────────────────────────────────
        let context = (0.60 * cap_ratio(self.context_markers, 4)
            + 0.40 * cap_ratio(self.grounding_refs, 3))
        .clamp(0.0, 1.0);

        // ── Control / specification ─ reward breadth of categories hit ───────
        let control = (0.26 * cap_ratio(self.constraints, 4)
            + 0.22 * cap_ratio(self.verification, 3)
            + 0.20 * cap_ratio(self.output_shape, 3)
            + 0.14 * cap_ratio(self.edge_cases, 2)
            + 0.10 * cap_ratio(self.scope, 2)
            + 0.08 * cap_ratio(self.safety, 2))
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

        // ── Clarity — trapezoid readability band on code-masked prose ───────
        let words_per_sentence = n / self.sentences as f64;
        let syll_per_word = self.syllables as f64 / n;
        let fk_grade = 0.39 * words_per_sentence + 11.8 * syll_per_word - 15.59;
        let flesch_reading_ease = 206.835 - 1.015 * words_per_sentence - 84.6 * syll_per_word;
        let complex_ratio = self.polysyllables as f64 / n;
        let gunning_fog = 0.4 * (words_per_sentence + 100.0 * complex_ratio);
        let clarity = clarity_band(fk_grade, flesch_reading_ease, self.words);

        // ── Adequacy — length sweet spot (additive, gentle) ─────────────────
        let adequacy = length_adequacy(self.words);

        PromptScoreBreakdown {
            specificity,
            control,
            context,
            structure,
            diversity,
            clarity,
            adequacy,
            flesch_reading_ease,
            fk_grade,
            gunning_fog,
        }
    }

    /// Up to two diagnostic flags, weakest qualifying dimension first.
    fn flags(&self, b: &PromptScoreBreakdown) -> Vec<Flag> {
        let mut out = Vec::new();
        if self.words < 15 {
            out.push(Flag::TooShort);
        }
        // Candidate (signal, flag) pairs, lowest signal surfaced first.
        let mut cands: Vec<(f64, Flag)> = vec![
            (b.specificity, Flag::Vague),
            (b.context, Flag::WeakContext),
            (b.control, Flag::WeakVerification),
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
            if sig < 0.35 {
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
    "replace", "wire", "integrate", "parse", "render", "validate", "handle",
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

const CONSTRAINTS: &[&str] = &[
    "must", "should", "only", "without", "avoid", "never", "always", "do not",
    "don't", "dont", "at least", "at most", "no more than", "limit", "require",
    "offline", "keep", "preserve", "backward", "compatible", "minimal",
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

/// Count sentence-ish units. Splits on `.`/`!`/`?`/`;` and newlines (each
/// bulleted line counts as a clause). Consecutive terminators collapse.
fn count_sentences(text: &str) -> usize {
    let mut count = 0usize;
    let mut in_sentence = false;
    for ch in text.chars() {
        if matches!(ch, '.' | '!' | '?' | '\n' | ';') {
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

/// Balance gates (v1.1, locked with the codex agent over i5h). You cannot look
/// mature on one axis alone, but the policy distinguishes the two specification
/// axes:
///
/// * `control` — *did the engineer bound the agent?* — is a **hard gate**: a
///   prompt that sets no constraints / acceptance criteria is capped below
///   "advanced" regardless of how concrete it is.
/// * `context` — *did they explain why?* — is **soft**: a prompt that is already
///   both specific (`>=0.6`) and bounded (`control>=0.5`) is a legitimate
///   *tactical* ask ("run `cargo test`, fix the clippy warning in `src/foo.rs`")
///   and must not be dragged into mediocrity merely for omitting background —
///   the agent often already holds the repo / h5i context. Weak context still
///   surfaces as a flag; it just no longer hard-caps a crisp tactical prompt.
///
/// A low specificity additionally caps at 79 (you can't be "exemplary" while
/// vague).
fn apply_balance_gates(score: f64, b: &PromptScoreBreakdown) -> f64 {
    let tactical = b.specificity >= 0.6 && b.control >= 0.5;
    let mut s = score;
    if b.control < 0.35 || (b.context < 0.35 && !tactical) {
        s = s.min(69.0);
    }
    if b.specificity < 0.45 {
        s = s.min(79.0);
    }
    s
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
    fn weights_sum_to_one() {
        assert!((WEIGHTS.sum() - 1.0).abs() < 1e-9, "weights sum = {}", WEIGHTS.sum());
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
        assert!(rich.breakdown.specificity > 0.4);
        assert!(rich.breakdown.control > 0.3);
        assert!(rich.level >= MaturityLevel::Proficient);
    }

    /// Build a breakdown with the three gate-relevant axes set and everything
    /// else neutral — lets the gate policy be tested in isolation.
    fn bd(specificity: f64, control: f64, context: f64) -> PromptScoreBreakdown {
        PromptScoreBreakdown {
            specificity,
            control,
            context,
            ..PromptScoreBreakdown::zero()
        }
    }

    #[test]
    fn gate_control_is_a_hard_cap() {
        // No constraints/acceptance → capped at 69 no matter how concrete.
        assert_eq!(apply_balance_gates(90.0, &bd(0.9, 0.2, 0.9)), 69.0);
    }

    #[test]
    fn gate_weak_context_caps_a_non_tactical_prompt() {
        // Thin on both context and specificity → capped.
        assert_eq!(apply_balance_gates(85.0, &bd(0.5, 0.6, 0.1)), 69.0);
    }

    #[test]
    fn gate_exempts_specific_and_bounded_tactical_prompt() {
        // v1.1: specific (>=.6) AND bounded (control>=.5) with weak context is a
        // legitimate tactical ask — NOT capped for missing "why".
        assert_eq!(apply_balance_gates(85.0, &bd(0.7, 0.6, 0.1)), 85.0);
        // …but if control slips below .5 it's no longer tactical → capped again.
        assert_eq!(apply_balance_gates(85.0, &bd(0.7, 0.45, 0.1)), 69.0);
    }

    #[test]
    fn gate_low_specificity_caps_at_79() {
        assert_eq!(apply_balance_gates(95.0, &bd(0.4, 0.9, 0.9)), 79.0);
    }

    #[test]
    fn tactical_prompt_keeps_weak_context_flag() {
        // The exemption must not hide the diagnostic — a concrete, bounded,
        // context-free prompt should still *flag* weak context even though it's
        // no longer hard-capped.
        let s = score_prompt(
            "Harden `Invoice::finalize()` in src/billing.rs in three steps:\n\
             - Reject a zero-quantity item by returning `Err(BillingError::EmptyLine)`.\n\
             - Round each subtotal to two decimals, then sanitize the note to drop \
               control characters.\n\
             - Add focused checks: an empty item, a rounding boundary, and a \
               happy-path total; assert the exact cents. Keep the signature stable \
               and edit only billing.rs.",
        );
        assert!(s.breakdown.specificity >= 0.6 && s.breakdown.control >= 0.5);
    }

    #[test]
    fn keyword_stuffing_is_capped() {
        let spam = score_prompt(
            "must must should ensure test test verify return format handle error \
             case must test format must test format edge case must verify only",
        );
        // balance gate (no context) + repetition penalty keep it out of advanced
        assert!(spam.score <= 69.0, "stuffed prompt scored {}", spam.score);
    }

    #[test]
    fn tactical_context_free_prompt_is_exempt_from_context_gate() {
        // v1.1 end-to-end: this prompt is concrete AND bounded (tactical) but
        // states no "why". The context gate must NOT cap it — verify the real
        // feature profile is *exempt* (a high base score survives the gate),
        // which is the opposite of the pre-v1.1 behavior.
        let s = score_prompt(
            "Edit `foo()` in src/a.rs and `bar()` in src/b.rs. Must return JSON. \
             Add tests. Do not change signatures. Only touch those two files.",
        );
        assert!(s.breakdown.context < 0.35, "prompt should read as context-free");
        assert!(
            s.breakdown.specificity >= 0.6 && s.breakdown.control >= 0.5,
            "prompt should qualify as tactical (spec {:.2}, control {:.2})",
            s.breakdown.specificity,
            s.breakdown.control,
        );
        // A high base score is NOT clamped to 69 for this tactical profile.
        assert_eq!(apply_balance_gates(85.0, &s.breakdown), 85.0);
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
    fn weak_words_lower_specificity() {
        let crisp = score_prompt(
            "Add a `retry()` wrapper around the HTTP call in src/net.rs with a 3x cap.",
        );
        let weak = score_prompt(
            "Make the thing handle stuff appropriately and maybe make it better somehow.",
        );
        assert!(crisp.breakdown.specificity > weak.breakdown.specificity);
    }

    #[test]
    fn branch_aggregation_is_length_weighted_mean() {
        let a = "fix it"; // short, low
        let b = "Add a unit test for `foo()` in src/foo.rs and ensure it passes \
                 without changing the public signature, covering the empty input case.";
        let branch = score_branch(vec![a, b], 2);
        assert_eq!(branch.scored_prompts, 2);
        assert_eq!(branch.ai_commits, 2);
        assert!((branch.coverage - 1.0).abs() < 1e-9);
        // weighted toward the longer, better prompt
        let sa = score_prompt(a);
        let sb = score_prompt(b);
        let plain_mean = (sa.score + sb.score) / 2.0;
        assert!(branch.score > plain_mean, "length weighting should lift {} above {}", branch.score, plain_mean);
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
        let e = score_prompt("   ");
        assert_eq!(e.score, 0.0);
        assert_eq!(e.flags, vec![Flag::TooShort]);
        let short = score_prompt("fix bug");
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
        let expect: f64 = scored
            .iter()
            .map(|s| s.score * s.words.clamp(20, 250) as f64)
            .sum::<f64>()
            / wsum;
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

    #[test]
    fn long_prompt_does_not_panic_and_scores_in_range() {
        // Exercises the sliding-window MATTR well past the window size.
        let big = "Refactor the module carefully and add tests. ".repeat(120);
        let s = score_prompt(&big);
        assert!((0.0..=100.0).contains(&s.score));
        assert!(s.words > 500);
    }
}
