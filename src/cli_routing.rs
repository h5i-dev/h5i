//! Pure CLI noun/verb routing — the `h5i <noun> <verb> …` → legacy-argv
//! rewrite, extracted from `main.rs` so it is unit-testable (the binary's
//! `fn main` can't be).
//!
//! [`plan_noun_route`] is a pure function: it inspects argv and returns a
//! [`NounRoute`] describing what should happen, *without* printing or exiting.
//! The thin `rewrite_noun_argv` shell in `main.rs` turns a `NounRoute` into the
//! actual help-print / `process::exit` / rewritten-argv. Keeping the decision
//! pure means every branch (passthrough, rewrite, help, unknown-verb +
//! suggestion) is testable without spawning a process.

/// The noun groups that front the legacy verbs.
pub const NOUNS: &[&str] = &["capture", "recall", "audit", "share", "objects"];

/// What [`plan_noun_route`] decided argv should do. The caller (in `main.rs`)
/// renders the side effects; this type carries only data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NounRoute {
    /// Not a noun invocation — use the original argv unchanged.
    Passthrough,
    /// Rewritten argv (legacy form) to hand to clap.
    Rewritten(Vec<String>),
    /// Print the noun's verb listing and exit 0.
    Help { noun: String },
    /// Unknown verb under a known noun — print an error (with an optional
    /// did-you-mean `suggestion`) and exit 2.
    UnknownVerb {
        noun: String,
        verb: String,
        suggestion: Option<String>,
    },
}

/// Plan the rewrite of `h5i <noun> <verb> …` into the legacy form, purely.
///
/// Mirrors the original `rewrite_noun_argv` control flow exactly, but returns a
/// decision instead of printing/exiting:
/// - `argv` shorter than 2, or `argv[1]` not a noun → [`NounRoute::Passthrough`].
/// - `h5i help <noun>` or a missing/`--help`/`-h`/`help` verb → [`NounRoute::Help`].
/// - a known `(noun, verb)` → [`NounRoute::Rewritten`] with `[bin, …mapped, …rest]`.
/// - an unknown verb → [`NounRoute::UnknownVerb`] with the nearest known verb.
pub fn plan_noun_route(argv: &[String]) -> NounRoute {
    if argv.len() < 2 {
        return NounRoute::Passthrough;
    }
    // `h5i help <noun>` is a synonym for `h5i <noun> --help`.
    if argv[1] == "help"
        && argv
            .get(2)
            .map(|t| NOUNS.contains(&t.as_str()))
            .unwrap_or(false)
    {
        return NounRoute::Help {
            noun: argv[2].clone(),
        };
    }
    let noun = match argv[1].as_str() {
        s if NOUNS.contains(&s) => argv[1].clone(),
        _ => return NounRoute::Passthrough,
    };

    // No verb (or asking for help): show the noun's verb listing.
    if argv.len() < 3 || matches!(argv[2].as_str(), "--help" | "-h" | "help") {
        return NounRoute::Help { noun };
    }

    let verb = argv[2].as_str();
    let Some(mapped) = noun_alias(&noun, verb) else {
        let suggestion = nearest_verb(&noun, verb).map(|s| s.to_string());
        return NounRoute::UnknownVerb {
            noun,
            verb: verb.to_string(),
            suggestion,
        };
    };

    // Rebuild argv: [bin, ...mapped, ...rest]
    let mut out = Vec::with_capacity(argv.len() + mapped.len());
    out.push(argv[0].clone());
    for tok in mapped {
        out.push(tok.to_string());
    }
    out.extend(argv.iter().skip(3).cloned());
    NounRoute::Rewritten(out)
}

/// Return the verb under `noun` whose name is closest (Levenshtein ≤ 2) to `typo`.
pub fn nearest_verb(noun: &str, typo: &str) -> Option<&'static str> {
    let candidates: &[&'static str] = match noun {
        "capture" => &["commit", "claim", "memory", "run"],
        "recall" => &[
            "log", "blame", "diff", "context", "claims", "notes", "memory", "recap", "resume",
            "vibe", "object", "objects", "search",
        ],
        "audit" => &["review", "scan", "compliance", "policy", "vibe"],
        "share" => &[
            "push",
            "pull",
            "pr",
            "memory",
            "setup-remote",
            "migrate-remote",
        ],
        "objects" => &[
            "run", "put", "get", "list", "ls", "search", "gc", "pin", "unpin", "fsck", "push",
            "pull", "filters", "trust", "setup",
        ],
        _ => return None,
    };
    let typo_l = typo.to_lowercase();
    let mut best: Option<(usize, &'static str)> = None;
    for &c in candidates {
        let d = levenshtein(&typo_l, c);
        if d <= 2 && best.map(|(bd, _)| d < bd).unwrap_or(true) {
            best = Some((d, c));
        }
    }
    best.map(|(_, v)| v)
}

pub fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur: Vec<usize> = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            cur[j + 1] = (cur[j] + 1).min(prev[j + 1] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// Map `(noun, verb)` to the legacy argv tokens that implement it.
pub fn noun_alias(noun: &str, verb: &str) -> Option<&'static [&'static str]> {
    Some(match (noun, verb) {
        // ── capture ─────────────────────────────────────────────────────
        ("capture", "commit") => &["commit"],
        ("capture", "claim") => &["claims", "add"],
        ("capture", "claims") => &["claims", "add"],
        ("capture", "memory") => &["memory", "snapshot"],
        ("capture", "run") => &["objects", "run"],
        ("capture", "output") => &["objects", "run"],

        // ── recall ──────────────────────────────────────────────────────
        ("recall", "log") => &["log"],
        ("recall", "blame") => &["blame"],
        ("recall", "diff") => &["diff"],
        ("recall", "context") => &["context"],
        ("recall", "claims") => &["claims", "list"],
        ("recall", "claim") => &["claims", "list"],
        ("recall", "notes") => &["notes"],
        ("recall", "memory") => &["memory"],
        ("recall", "recap") => &["context", "recap"],
        ("recall", "resume") => &["resume"],
        ("recall", "vibe") => &["vibe"],
        ("recall", "object") => &["objects", "get"],
        ("recall", "objects") => &["objects", "list"],
        ("recall", "search") => &["objects", "search"],

        // ── audit ───────────────────────────────────────────────────────
        ("audit", "review") => &["notes", "review"],
        ("audit", "scan") => &["context", "scan"],
        ("audit", "compliance") => &["compliance"],
        ("audit", "policy") => &["policy"],
        ("audit", "vibe") => &["vibe"],
        ("audit", "notes") => &["notes", "review"],

        // ── share ───────────────────────────────────────────────────────
        ("share", "push") => &["push"],
        ("share", "pull") => &["pull"],
        ("share", "pr") => &["pr"],
        ("share", "memory") => &["memory"],
        ("share", "setup-remote") => &["setup-remote"],
        ("share", "migrate-remote") => &["migrate-remote"],

        // ── objects (token-reduction store maintenance) ──────────────────
        ("objects", "run") => &["objects", "run"],
        ("objects", "put") => &["objects", "put"],
        ("objects", "get") => &["objects", "get"],
        ("objects", "list") => &["objects", "list"],
        ("objects", "ls") => &["objects", "list"],
        ("objects", "search") => &["objects", "search"],
        ("objects", "gc") => &["objects", "gc"],
        ("objects", "pin") => &["objects", "pin"],
        ("objects", "unpin") => &["objects", "unpin"],
        ("objects", "fsck") => &["objects", "fsck"],
        ("objects", "push") => &["objects", "push"],
        ("objects", "pull") => &["objects", "pull"],
        ("objects", "filters") => &["objects", "filters"],
        ("objects", "rules") => &["objects", "filters"],
        ("objects", "trust") => &["objects", "trust"],
        ("objects", "setup") => &["objects", "setup"],

        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn too_short_or_non_noun_passes_through() {
        assert_eq!(plan_noun_route(&argv(&["h5i"])), NounRoute::Passthrough);
        assert_eq!(plan_noun_route(&argv(&["h5i", "log"])), NounRoute::Passthrough);
        assert_eq!(
            plan_noun_route(&argv(&["h5i", "commit", "-m", "x"])),
            NounRoute::Passthrough
        );
    }

    #[test]
    fn known_noun_verb_rewrites_to_legacy_and_keeps_rest() {
        assert_eq!(
            plan_noun_route(&argv(&["h5i", "capture", "commit", "-m", "x"])),
            NounRoute::Rewritten(argv(&["h5i", "commit", "-m", "x"]))
        );
        // Multi-token mapping + trailing args preserved in order.
        assert_eq!(
            plan_noun_route(&argv(&["h5i", "recall", "search", "needle", "--json"])),
            NounRoute::Rewritten(argv(&["h5i", "objects", "search", "needle", "--json"]))
        );
        assert_eq!(
            plan_noun_route(&argv(&["h5i", "capture", "claim", "fact"])),
            NounRoute::Rewritten(argv(&["h5i", "claims", "add", "fact"]))
        );
    }

    #[test]
    fn missing_or_help_verb_requests_help() {
        assert_eq!(
            plan_noun_route(&argv(&["h5i", "capture"])),
            NounRoute::Help { noun: "capture".into() }
        );
        for h in ["--help", "-h", "help"] {
            assert_eq!(
                plan_noun_route(&argv(&["h5i", "recall", h])),
                NounRoute::Help { noun: "recall".into() }
            );
        }
        // `h5i help <noun>` synonym.
        assert_eq!(
            plan_noun_route(&argv(&["h5i", "help", "share"])),
            NounRoute::Help { noun: "share".into() }
        );
        // `h5i help <non-noun>` is not our concern — pass through to clap.
        assert_eq!(
            plan_noun_route(&argv(&["h5i", "help", "log"])),
            NounRoute::Passthrough
        );
    }

    #[test]
    fn unknown_verb_suggests_nearest() {
        match plan_noun_route(&argv(&["h5i", "capture", "comit"])) {
            NounRoute::UnknownVerb { noun, verb, suggestion } => {
                assert_eq!(noun, "capture");
                assert_eq!(verb, "comit");
                assert_eq!(suggestion.as_deref(), Some("commit"));
            }
            other => panic!("expected UnknownVerb, got {other:?}"),
        }
        // A verb with no close match still reports UnknownVerb, suggestion None.
        match plan_noun_route(&argv(&["h5i", "audit", "zzzzzzzz"])) {
            NounRoute::UnknownVerb { suggestion, .. } => assert_eq!(suggestion, None),
            other => panic!("expected UnknownVerb, got {other:?}"),
        }
    }

    #[test]
    fn levenshtein_basic() {
        assert_eq!(levenshtein("commit", "commit"), 0);
        assert_eq!(levenshtein("comit", "commit"), 1);
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("abc", ""), 3);
    }

    #[test]
    fn nearest_verb_respects_distance_cap() {
        assert_eq!(nearest_verb("capture", "comit"), Some("commit"));
        assert_eq!(nearest_verb("capture", "xyz"), None); // > 2 edits from all
        assert_eq!(nearest_verb("not-a-noun", "anything"), None);
    }
}
