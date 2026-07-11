//! `h5i maturity` — CLI handler (migrated from main.rs).
#![allow(clippy::all)]
use crate::*;

pub fn run(text: Option<String>, oid: Option<String>, limit: usize, json: bool) -> anyhow::Result<()> {
    {
            use h5i_core::prompt_score as ps;

            // Serde shapes — kept inline (like `vibe`) so the score types stay
            // free of a serde dependency. `breakdown_json` mirrors the HTTP
            // `/api/prompt-score` fields so a CI consumer sees the same signal.
            #[derive(serde::Serialize)]
            struct BreakdownJson {
                objective: f64,
                grounding: f64,
                direction: f64,
                context: f64,
                examples: f64,
                structure: f64,
                diversity: f64,
                clarity: f64,
                adequacy: f64,
                evidence: f64,
                flesch_reading_ease: f64,
                fk_grade: f64,
                gunning_fog: f64,
            }
            let breakdown_json = |b: &ps::PromptScoreBreakdown| BreakdownJson {
                objective: b.objective,
                grounding: b.grounding,
                direction: b.direction,
                context: b.context,
                examples: b.examples,
                structure: b.structure,
                diversity: b.diversity,
                clarity: b.clarity,
                adequacy: b.adequacy,
                evidence: b.evidence,
                flesch_reading_ease: b.flesch_reading_ease,
                fk_grade: b.fk_grade,
                gunning_fog: b.gunning_fog,
            };
            // Human breakdown table: the three core slots, then the enrichment
            // signals, each as a 10-cell bar. Diagnostic only — never a keyword
            // checklist to stuff.
            let bar = |v: f64| {
                let filled = (v.clamp(0.0, 1.0) * 10.0).round() as usize;
                format!("{}{}", "█".repeat(filled), "░".repeat(10 - filled))
            };
            let print_breakdown = |b: &ps::PromptScoreBreakdown| {
                let rows: [(&str, f64); 9] = [
                    ("Objective (core)", b.objective),
                    ("Grounding (core)", b.grounding),
                    ("Direction (core)", b.direction),
                    ("Context", b.context),
                    ("Examples", b.examples),
                    ("Structure", b.structure),
                    ("Diversity", b.diversity),
                    ("Clarity", b.clarity),
                    ("Adequacy", b.adequacy),
                ];
                for (label, v) in rows {
                    println!(
                        "   {:<18} {} {:.2}",
                        label,
                        style(bar(v)).cyan(),
                        v
                    );
                }
                if b.evidence > 0.0 {
                    println!(
                        "   {:<18} {} {:.2} {}",
                        "Evidence (bonus)",
                        style(bar(b.evidence)).green(),
                        b.evidence,
                        style("(+bonus)").dim()
                    );
                }
            };

            if text.is_some() || oid.is_some() {
                // ── Single-prompt mode ──────────────────────────────────────
                let prompt = if let Some(t) = text {
                    t
                } else {
                    let repo = H5iRepository::open(".")?;
                    let oid_s = oid.expect("oid set when text is None");
                    let git_oid = git2::Oid::from_str(&oid_s)
                        .map_err(|_| anyhow::anyhow!("`{oid_s}` is not a valid git OID"))?;
                    repo.load_h5i_record(git_oid)
                        .ok()
                        .and_then(|r| r.ai_metadata)
                        .map(|ai| ai.prompt)
                        .filter(|p| !p.is_empty())
                        .ok_or_else(|| {
                            anyhow::anyhow!("commit {oid_s} has no captured prompt to score")
                        })?
                };
                let s = ps::score_prompt(&prompt);
                if json {
                    #[derive(serde::Serialize)]
                    struct SingleJson {
                        mode: &'static str,
                        score: f64,
                        level: &'static str,
                        words: usize,
                        #[serde(skip_serializing_if = "Option::is_none")]
                        unscored: Option<&'static str>,
                        flags: Vec<&'static str>,
                        breakdown: Option<BreakdownJson>,
                    }
                    let out = SingleJson {
                        mode: "prompt",
                        score: s.score,
                        level: if s.is_unscored() {
                            "unscored"
                        } else {
                            s.level.label()
                        },
                        words: s.words,
                        unscored: s.unscored,
                        flags: s.flags.iter().map(|f| f.label()).collect(),
                        breakdown: if s.is_unscored() {
                            None
                        } else {
                            Some(breakdown_json(&s.breakdown))
                        },
                    };
                    println!("{}", serde_json::to_string_pretty(&out)?);
                } else if let Some(reason) = s.unscored {
                    println!(
                        "🧠 {} — {}",
                        style("Prompt maturity: unscored").yellow().bold(),
                        reason
                    );
                } else {
                    println!(
                        "🧠 {}  {} {}   {}",
                        style(format!("Prompt maturity: {:.1}/100", s.score))
                            .bold(),
                        s.level.emoji(),
                        style(s.level.label()).cyan(),
                        style(format!("({} words)", s.words)).dim()
                    );
                    if !s.flags.is_empty() {
                        let flags: Vec<&str> = s.flags.iter().map(|f| f.label()).collect();
                        println!("   {} {}", style("flags:").dim(), flags.join(", "));
                    }
                    print_breakdown(&s.breakdown);
                }
            } else {
                // ── Branch mode: roll up every AI-commit prompt on base..HEAD ─
                let workdir = std::env::current_dir()?;
                let repo = H5iRepository::open(&workdir)?;
                let base_oid = h5i_core::pr::detect_base_oid(repo.git(), &workdir);
                let records = repo.h5i_log_since(base_oid, limit)?;
                let ai_count = records
                    .iter()
                    .filter(|r| r.ai_metadata.is_some())
                    .count();
                let prompts: Vec<&str> = records
                    .iter()
                    .filter_map(|r| r.ai_metadata.as_ref())
                    .map(|m| m.prompt.as_str())
                    .collect();
                let branch = ps::score_branch(prompts, ai_count);
                if json {
                    #[derive(serde::Serialize)]
                    struct BranchJson {
                        mode: &'static str,
                        score: f64,
                        level: &'static str,
                        scored_prompts: usize,
                        ai_commits: usize,
                        coverage: f64,
                        low_confidence: bool,
                        flags: Vec<&'static str>,
                        breakdown: Option<BreakdownJson>,
                    }
                    let out = BranchJson {
                        mode: "branch",
                        score: branch.score,
                        level: if branch.is_empty() {
                            "unscored"
                        } else {
                            branch.level.label()
                        },
                        scored_prompts: branch.scored_prompts,
                        ai_commits: branch.ai_commits,
                        coverage: branch.coverage,
                        low_confidence: branch.low_confidence,
                        flags: branch.flags.iter().map(|f| f.label()).collect(),
                        breakdown: if branch.is_empty() {
                            None
                        } else {
                            Some(breakdown_json(&branch.breakdown))
                        },
                    };
                    println!("{}", serde_json::to_string_pretty(&out)?);
                } else if branch.is_empty() {
                    println!(
                        "🧠 {}",
                        style("No scorable AI-commit prompts on this branch.")
                            .yellow()
                    );
                    println!(
                        "   {}",
                        style("Commit with `h5i capture commit` so the prompt is captured.")
                            .dim()
                    );
                } else {
                    println!(
                        "🧠 {}  {} {}",
                        style(format!("Prompt maturity: {:.1}/100", branch.score))
                            .bold(),
                        branch.level.emoji(),
                        style(branch.level.label()).cyan()
                    );
                    println!(
                        "   {} {}/{} AI commits scored ({:.0}% coverage){}",
                        style("coverage:").dim(),
                        branch.scored_prompts,
                        branch.ai_commits,
                        branch.coverage * 100.0,
                        if branch.low_confidence {
                            format!(" {}", style("· low confidence").yellow())
                        } else {
                            String::new()
                        }
                    );
                    if !branch.flags.is_empty() {
                        let flags: Vec<&str> =
                            branch.flags.iter().map(|f| f.label()).collect();
                        println!("   {} {}", style("common flags:").dim(), flags.join(", "));
                    }
                    print_breakdown(&branch.breakdown);
                }
            }
        }
    Ok(())
}
