//! Declarative, per-command output filters (phase 2 of token reduction).
//!
//! This is an h5i port of the TOML filter engine from **rtk**
//! (<https://github.com/rtk-ai/rtk>, Apache-2.0, © Patrick Szymkowiak). The
//! engine semantics and the built-in rule files under `assets/filters/` are
//! derived from rtk with modifications; see `assets/filters/NOTICE`.
//!
//! Where [`crate::token_filter`]'s coded adapters (pytest/cargo/git) give the
//! deepest summaries for a few tools, this engine covers the *long tail*: a
//! declarative rule per command (gcc, make, terraform, eslint, …) that an
//! author can add without touching Rust. Each rule is a small pipeline:
//!
//!   1. `strip_ansi`            — remove ANSI escape codes
//!   2. `replace`              — regex substitutions, line-by-line, chained
//!   3. `match_output`         — short-circuit: if the whole blob matches a
//!      pattern, return its message (with an `unless` guard so errors aren't
//!      swallowed)
//!   4. `strip_lines_matching` / `keep_lines_matching` — line regex filter
//!   5. `truncate_lines_at`    — cap each line to N chars
//!   6. `head_lines` / `tail_lines` — keep first/last N lines
//!   7. `max_lines`            — absolute line cap
//!   8. `on_empty`             — message when the result is blank
//!
//! Rules ship with inline golden tests (`[[tests.<name>]]`); [`run_golden_tests`]
//! executes every one, which is how we prove the port matches rtk's behavior.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::{Regex, RegexSet};
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};

/// The built-in rule files, embedded at compile time from `assets/filters/`.
#[derive(RustEmbed)]
#[folder = "assets/filters/"]
struct BuiltinFilters;

/// Embedded paths that are rule files (ignore NOTICE, README, …).
fn rule_paths() -> Vec<String> {
    let mut names: Vec<String> = BuiltinFilters::iter()
        .map(|c| c.to_string())
        .filter(|p| p.ends_with(".toml"))
        .collect();
    names.sort();
    names
}

// ── On-disk schema (one rule file) ──────────────────────────────────────────

#[derive(Deserialize)]
struct FilterFile {
    /// Optional; individual built-in files omit it (it's a whole-bundle notion).
    #[allow(dead_code)]
    #[serde(default)]
    schema_version: Option<u32>,
    #[serde(default)]
    filters: BTreeMap<String, FilterDef>,
    /// Inline golden tests, keyed by filter name.
    #[serde(default)]
    tests: BTreeMap<String, Vec<FilterTest>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FilterDef {
    #[serde(default)]
    description: Option<String>,
    match_command: String,
    #[serde(default)]
    strip_ansi: bool,
    #[serde(default)]
    replace: Vec<ReplaceRule>,
    #[serde(default)]
    match_output: Vec<MatchOutputRule>,
    #[serde(default)]
    strip_lines_matching: Vec<String>,
    #[serde(default)]
    keep_lines_matching: Vec<String>,
    truncate_lines_at: Option<usize>,
    head_lines: Option<usize>,
    tail_lines: Option<usize>,
    max_lines: Option<usize>,
    on_empty: Option<String>,
    /// rtk uses this to merge stderr before filtering. h5i always captures the
    /// combined stdout+stderr blob, so it's accepted but has no effect here.
    #[serde(default)]
    #[allow(dead_code)]
    filter_stderr: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplaceRule {
    pattern: String,
    replacement: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MatchOutputRule {
    pattern: String,
    message: String,
    #[serde(default)]
    unless: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FilterTest {
    #[allow(dead_code)]
    name: String,
    input: String,
    expected: String,
}

// ── Compiled (regexes ready) ────────────────────────────────────────────────

enum LineFilter {
    None,
    Strip(RegexSet),
    Keep(RegexSet),
}

struct CompiledReplace {
    pattern: Regex,
    replacement: String,
}

struct CompiledMatchOutput {
    pattern: Regex,
    message: String,
    unless: Option<Regex>,
}

/// A parsed, compiled rule ready to run.
pub struct CompiledFilter {
    pub name: String,
    pub description: Option<String>,
    pub match_pattern: String,
    match_regex: Regex,
    strip_ansi: bool,
    replace: Vec<CompiledReplace>,
    match_output: Vec<CompiledMatchOutput>,
    line_filter: LineFilter,
    truncate_lines_at: Option<usize>,
    head_lines: Option<usize>,
    tail_lines: Option<usize>,
    max_lines: Option<usize>,
    on_empty: Option<String>,
}

fn compile(name: String, def: FilterDef) -> Result<CompiledFilter, String> {
    let match_regex = Regex::new(&def.match_command)
        .map_err(|e| format!("invalid match_command '{}': {e}", def.match_command))?;
    let replace = def
        .replace
        .into_iter()
        .map(|r| {
            Ok(CompiledReplace {
                pattern: Regex::new(&r.pattern)
                    .map_err(|e| format!("invalid replace pattern '{}': {e}", r.pattern))?,
                replacement: r.replacement,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let match_output = def
        .match_output
        .into_iter()
        .map(|r| {
            let pattern = Regex::new(&r.pattern)
                .map_err(|e| format!("invalid match_output pattern '{}': {e}", r.pattern))?;
            let unless = match r.unless {
                Some(u) => Some(
                    Regex::new(&u)
                        .map_err(|e| format!("invalid match_output unless '{u}': {e}"))?,
                ),
                None => None,
            };
            Ok(CompiledMatchOutput {
                pattern,
                message: r.message,
                unless,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    // strip takes precedence over keep when both are present (matches rtk).
    let line_filter = if !def.strip_lines_matching.is_empty() {
        LineFilter::Strip(
            RegexSet::new(&def.strip_lines_matching)
                .map_err(|e| format!("invalid strip_lines_matching: {e}"))?,
        )
    } else if !def.keep_lines_matching.is_empty() {
        LineFilter::Keep(
            RegexSet::new(&def.keep_lines_matching)
                .map_err(|e| format!("invalid keep_lines_matching: {e}"))?,
        )
    } else {
        LineFilter::None
    };
    Ok(CompiledFilter {
        name,
        description: def.description,
        match_pattern: def.match_command,
        match_regex,
        strip_ansi: def.strip_ansi,
        replace,
        match_output,
        line_filter,
        truncate_lines_at: def.truncate_lines_at,
        head_lines: def.head_lines,
        tail_lines: def.tail_lines,
        max_lines: def.max_lines,
        on_empty: def.on_empty,
    })
}

// ── rtk-compatible primitives ───────────────────────────────────────────────
//
// Deliberately mirror rtk's helpers byte-for-byte so the imported golden tests
// pass unchanged. (h5i's own `token_filter::strip_ansi` is richer — it also
// collapses CR progress bars — but the rule golden cases were written against
// rtk's simpler CSI strip.)

fn ansi_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\x1b\[[0-9;]*[a-zA-Z]").unwrap())
}

fn strip_ansi(text: &str) -> String {
    ansi_re().replace_all(text, "").to_string()
}

/// Char-based truncation with a trailing `...` (rtk semantics).
fn truncate(s: &str, max_len: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max_len {
        s.to_string()
    } else if max_len < 3 {
        "...".to_string()
    } else {
        format!("{}...", s.chars().take(max_len - 3).collect::<String>())
    }
}

/// Apply a compiled rule to raw output. Pure `String -> String`, 8 stages.
pub fn apply_filter(filter: &CompiledFilter, stdout: &str) -> String {
    let mut lines: Vec<String> = stdout.lines().map(String::from).collect();

    // 1. strip_ansi
    if filter.strip_ansi {
        lines = lines.iter().map(|l| strip_ansi(l)).collect();
    }

    // 2. replace — line-by-line, rules chained
    if !filter.replace.is_empty() {
        lines = lines
            .into_iter()
            .map(|mut line| {
                for rule in &filter.replace {
                    line = rule
                        .pattern
                        .replace_all(&line, rule.replacement.as_str())
                        .into_owned();
                }
                line
            })
            .collect();
    }

    // 3. match_output — short-circuit on full-blob match (first rule wins)
    if !filter.match_output.is_empty() {
        let blob = lines.join("\n");
        for rule in &filter.match_output {
            if rule.pattern.is_match(&blob) {
                if let Some(ref unless_re) = rule.unless {
                    if unless_re.is_match(&blob) {
                        continue;
                    }
                }
                return rule.message.clone();
            }
        }
    }

    // 4. strip OR keep (mutually exclusive)
    match &filter.line_filter {
        LineFilter::Strip(set) => lines.retain(|l| !set.is_match(l)),
        LineFilter::Keep(set) => lines.retain(|l| set.is_match(l)),
        LineFilter::None => {}
    }

    // 5. truncate_lines_at
    if let Some(max_chars) = filter.truncate_lines_at {
        lines = lines.into_iter().map(|l| truncate(&l, max_chars)).collect();
    }

    // 6. head + tail
    let total = lines.len();
    if let (Some(head), Some(tail)) = (filter.head_lines, filter.tail_lines) {
        if total > head + tail {
            let mut result = lines[..head].to_vec();
            result.push(format!("... ({} lines omitted)", total - head - tail));
            result.extend_from_slice(&lines[total - tail..]);
            lines = result;
        }
    } else if let Some(head) = filter.head_lines {
        if total > head {
            lines.truncate(head);
            lines.push(format!("... ({} lines omitted)", total - head));
        }
    } else if let Some(tail) = filter.tail_lines {
        if total > tail {
            let omitted = total - tail;
            lines = lines[omitted..].to_vec();
            lines.insert(0, format!("... ({omitted} lines omitted)"));
        }
    }

    // 7. max_lines — absolute cap after head/tail
    if let Some(max) = filter.max_lines {
        if lines.len() > max {
            let truncated = lines.len() - max;
            lines.truncate(max);
            lines.push(format!("... ({truncated} lines truncated)"));
        }
    }

    // 8. on_empty
    let result = lines.join("\n");
    if result.trim().is_empty() {
        if let Some(ref msg) = filter.on_empty {
            return msg.clone();
        }
    }
    result
}

// ── Registry ────────────────────────────────────────────────────────────────

/// All compiled built-in rules, plus a record of any that failed to parse.
pub struct Registry {
    pub filters: Vec<CompiledFilter>,
    /// (file, error) for any rule file that failed to parse/compile.
    pub errors: Vec<(String, String)>,
}

impl Registry {
    fn load() -> Registry {
        let mut filters = Vec::new();
        let mut errors = Vec::new();
        // Iterate files in a stable (path-sorted) order, then — below — sort all
        // compiled filters by name. This reproduces rtk's effective match order:
        // rtk concatenates every rule file into one TOML parsed into a single
        // `BTreeMap<name, _>`, so its first-match-wins is global-alphabetical by
        // filter name. We match that exactly. (Built-in `match_command` patterns
        // are anchored per tool — `^gcc\b`, `^make\b`, … — so cross-rule overlap
        // is rare; the name order only matters if two patterns ever overlap.)
        for path in rule_paths() {
            let Some(file) = BuiltinFilters::get(&path) else {
                continue;
            };
            let Ok(text) = std::str::from_utf8(&file.data) else {
                errors.push((path.clone(), "not valid UTF-8".into()));
                continue;
            };
            match parse_file(text) {
                Ok(mut fs) => filters.append(&mut fs),
                Err(e) => errors.push((path.clone(), e)),
            }
        }
        // Global name-sort = rtk's concatenated-BTreeMap order (see above).
        filters.sort_by(|a, b| a.name.cmp(&b.name));
        Registry { filters, errors }
    }

    /// First rule whose `match_command` matches the command string, if any.
    pub fn find(&self, command: &str) -> Option<&CompiledFilter> {
        self.filters.iter().find(|f| f.match_regex.is_match(command))
    }
}

fn parse_file(text: &str) -> Result<Vec<CompiledFilter>, String> {
    let file: FilterFile = toml::from_str(text).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for (name, def) in file.filters {
        out.push(compile(name, def)?);
    }
    Ok(out)
}

/// The process-wide registry of built-in rules (compiled once).
pub fn registry() -> &'static Registry {
    static REG: OnceLock<Registry> = OnceLock::new();
    REG.get_or_init(Registry::load)
}

/// Build the command string a rule's `match_command` is tested against:
/// the argv joined by spaces, with `argv[0]` reduced to its basename so a
/// fully-qualified path (`/usr/bin/gcc`) still matches `^gcc\b`.
fn command_string(cmd: &[String]) -> String {
    if cmd.is_empty() {
        return String::new();
    }
    let prog = cmd[0].rsplit('/').next().unwrap_or(&cmd[0]);
    if cmd.len() == 1 {
        prog.to_string()
    } else {
        format!("{} {}", prog, cmd[1..].join(" "))
    }
}

/// Summarize `output` using the first matching rule. `trusted_project_file`, when
/// `Some`, is a path to a *trust-verified* `.h5i/filters.toml` whose rules are
/// tried **before** the built-ins (so a project can override). The caller is
/// responsible for the trust check (see [`trust_status`]); this function never
/// loads an untrusted file. Returns `(summary, matched_name)`, or `None`.
pub fn summarize_with_rules(
    cmd: &[String],
    output: &str,
    trusted_project_file: Option<&Path>,
) -> Option<(String, String)> {
    let command = command_string(cmd);
    if command.is_empty() {
        return None;
    }
    // Project-local rules first (already trust-checked by the caller).
    if let Some(pf) = trusted_project_file {
        if let Ok(text) = std::fs::read_to_string(pf) {
            if let Ok(filters) = parse_file(&text) {
                if let Some(f) = filters.iter().find(|f| f.match_regex.is_match(&command)) {
                    return Some((apply_filter(f, output), format!("{} (project)", f.name)));
                }
            }
        }
    }
    let filter = registry().find(&command)?;
    Some((apply_filter(filter, output), filter.name.clone()))
}

// ── Trust-gated project-local rules ──────────────────────────────────────────
//
// A repo may ship `.h5i/filters.toml` with its own rules. That file is untrusted
// input: a malicious rule could use `match_output` to hide real failures (e.g.
// always print "ok"), tricking an agent. So project rules are applied ONLY after
// the user has explicitly trusted the file's *current* content; any later edit
// re-arms the gate. Set `H5I_TRUST_FILTERS=1` to override (CI/automation).

/// Path to a project's local rule file (in the working tree, not `.git`).
pub fn project_filters_path(workdir: &Path) -> PathBuf {
    workdir.join(".h5i").join("filters.toml")
}

fn trust_record_path(h5i_root: &Path) -> PathBuf {
    h5i_root.join("trusted_filters.json")
}

#[derive(Serialize, Deserialize, Default)]
struct TrustRecord {
    path: String,
    sha256: String,
}

/// Trust state of a project's `.h5i/filters.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustStatus {
    /// No project rule file present.
    NoFile,
    /// File present but never trusted.
    Untrusted,
    /// File changed since it was trusted (re-review required).
    Changed,
    /// File matches the trusted hash.
    Trusted,
    /// `H5I_TRUST_FILTERS` is set — applied without a stored hash.
    EnvOverride,
}

fn env_trust_override() -> bool {
    std::env::var("H5I_TRUST_FILTERS")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Compute the current trust status of the project rule file.
pub fn trust_status(workdir: &Path, h5i_root: &Path) -> TrustStatus {
    let p = project_filters_path(workdir);
    if !p.is_file() {
        return TrustStatus::NoFile;
    }
    if env_trust_override() {
        return TrustStatus::EnvOverride;
    }
    let Ok(content) = std::fs::read(&p) else {
        return TrustStatus::Untrusted;
    };
    let hash = crate::objects::sha256_hex(&content);
    match read_trust_record(h5i_root) {
        Some(rec) if rec.sha256 == hash => TrustStatus::Trusted,
        Some(_) => TrustStatus::Changed,
        None => TrustStatus::Untrusted,
    }
}

fn read_trust_record(h5i_root: &Path) -> Option<TrustRecord> {
    let raw = std::fs::read_to_string(trust_record_path(h5i_root)).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Record the current content of the project rule file as trusted. Returns the
/// hash that was pinned.
pub fn trust(workdir: &Path, h5i_root: &Path) -> Result<String, String> {
    let p = project_filters_path(workdir);
    let content = std::fs::read(&p).map_err(|e| format!("read {}: {e}", p.display()))?;
    // Validate it parses before trusting, so a broken file can't be pinned.
    let text = String::from_utf8_lossy(&content);
    parse_file(&text).map_err(|e| format!("{} is not a valid rule file: {e}", p.display()))?;
    let hash = crate::objects::sha256_hex(&content);
    let rec = TrustRecord {
        path: p.display().to_string(),
        sha256: hash.clone(),
    };
    let json = serde_json::to_string_pretty(&rec).map_err(|e| e.to_string())?;
    std::fs::write(trust_record_path(h5i_root), json)
        .map_err(|e| format!("write trust record: {e}"))?;
    Ok(hash)
}

/// Remove the trust record (project rules will no longer be applied).
pub fn untrust(h5i_root: &Path) -> Result<(), String> {
    let p = trust_record_path(h5i_root);
    if p.exists() {
        std::fs::remove_file(&p).map_err(|e| format!("remove trust record: {e}"))?;
    }
    Ok(())
}

/// Summary of one rule in a project file, for the `trust` review prompt.
pub struct RuleSummary {
    pub name: String,
    pub match_pattern: String,
    /// True if the rule has a `match_output` that can short-circuit to a fixed
    /// message *without* an `unless` guard — i.e. it could mask real failures.
    pub can_hide_output: bool,
}

/// Parse a project rule file and describe its rules (for human review before
/// trusting). Returns an error if the file doesn't parse.
pub fn describe_file(path: &Path) -> Result<Vec<RuleSummary>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let file: FilterFile = toml::from_str(&text).map_err(|e| e.to_string())?;
    Ok(file
        .filters
        .into_iter()
        .map(|(name, def)| {
            let can_hide_output = def.match_output.iter().any(|r| r.unless.is_none());
            RuleSummary {
                name,
                match_pattern: def.match_command,
                can_hide_output,
            }
        })
        .collect())
}

/// `(name, description, match_pattern)` for every built-in rule, name-sorted.
pub fn list_filters() -> Vec<(String, String, String)> {
    registry()
        .filters
        .iter()
        .map(|f| {
            (
                f.name.clone(),
                f.description.clone().unwrap_or_default(),
                f.match_pattern.clone(),
            )
        })
        .collect()
}

/// One failing golden case.
pub struct GoldenFailure {
    pub filter: String,
    pub test: String,
    pub expected: String,
    pub actual: String,
}

/// Run every inline `[[tests.<name>]]` golden case across all built-in rule
/// files. Returns `(passed, failures)`. This is the fidelity check: a passing
/// run means our engine reproduces rtk's documented behavior for these rules.
pub fn run_golden_tests() -> (usize, Vec<GoldenFailure>) {
    let mut passed = 0;
    let mut failures = Vec::new();
    for path in rule_paths() {
        let Some(file) = BuiltinFilters::get(&path) else {
            continue;
        };
        let Ok(text) = std::str::from_utf8(&file.data) else {
            continue;
        };
        let Ok(parsed) = toml::from_str::<FilterFile>(text) else {
            continue;
        };
        // Compile the filters in this file by name for the test lookup.
        let mut compiled: BTreeMap<String, CompiledFilter> = BTreeMap::new();
        if let Ok(file2) = toml::from_str::<FilterFile>(text) {
            for (name, def) in file2.filters {
                if let Ok(cf) = compile(name.clone(), def) {
                    compiled.insert(name, cf);
                }
            }
        }
        for (fname, cases) in parsed.tests {
            let Some(cf) = compiled.get(&fname) else {
                continue;
            };
            for case in cases {
                let actual = apply_filter(cf, &case.input);
                // rtk compares with trimmed equality (TOML triple-strings carry
                // leading/trailing newlines).
                if actual.trim() == case.expected.trim() {
                    passed += 1;
                } else {
                    failures.push(GoldenFailure {
                        filter: fname.clone(),
                        test: case.name,
                        expected: case.expected.trim().to_string(),
                        actual: actual.trim().to_string(),
                    });
                }
            }
        }
    }
    (passed, failures)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_rules_all_parse() {
        let reg = registry();
        assert!(
            reg.errors.is_empty(),
            "rule files failed to parse: {:?}",
            reg.errors
        );
        assert!(reg.filters.len() >= 50, "expected the full rule set, got {}", reg.filters.len());
    }

    #[test]
    fn builtin_golden_tests_pass() {
        let (passed, failures) = run_golden_tests();
        assert!(passed > 0, "no golden tests ran");
        assert!(
            failures.is_empty(),
            "{} golden case(s) failed, e.g. [{}/{}]\n--- expected ---\n{}\n--- actual ---\n{}",
            failures.len(),
            failures.first().map(|f| f.filter.as_str()).unwrap_or(""),
            failures.first().map(|f| f.test.as_str()).unwrap_or(""),
            failures.first().map(|f| f.expected.as_str()).unwrap_or(""),
            failures.first().map(|f| f.actual.as_str()).unwrap_or(""),
        );
    }

    #[test]
    fn command_string_normalizes_argv0_basename() {
        assert_eq!(command_string(&["/usr/bin/gcc".into(), "-O2".into(), "x.c".into()]), "gcc -O2 x.c");
        assert_eq!(command_string(&["make".into()]), "make");
    }

    #[test]
    fn matches_and_filters_a_known_command() {
        // gcc is one of the imported rules; an include-chain note is stripped,
        // the error is kept.
        let out = "In file included from /usr/include/stdio.h:42:\nmain.c:10:5: error: use of undeclared identifier 'foo'\n";
        let res = summarize_with_rules(&["gcc".into(), "main.c".into()], out, None);
        let (summary, name) = res.expect("gcc rule should match");
        assert_eq!(name, "gcc");
        assert!(summary.contains("error: use of undeclared identifier 'foo'"));
        assert!(!summary.contains("In file included from"));
    }

    #[test]
    fn unknown_command_returns_none() {
        assert!(summarize_with_rules(&["totally-unknown-tool-xyz".into()], "hi", None).is_none());
    }

    /// `match_command` routing for common invocations — guards against a broken
    /// regex (e.g. the gradle rule once required `gradlew` twice and matched
    /// nothing). Golden tests exercise `apply_filter` only, so routing needs its
    /// own coverage.
    #[test]
    fn known_commands_route_to_expected_rules() {
        let cases: &[(&[&str], &str)] = &[
            (&["gradle", "build"], "gradle"),
            (&["gradlew", "build"], "gradle"),
            (&["./gradlew", "build"], "gradle"),
            (&["gcc", "-O2", "main.c"], "gcc"),
            (&["make", "all"], "make"),
            // Narrowed ecosystem rules: noisy subcommands route…
            (&["npm", "run", "build"], "npm"),
            (&["pnpm", "install"], "pnpm"),
            (&["yarn"], "yarn"),
            (&["go", "test", "./..."], "go"),
            (&["docker", "build", "."], "docker-build"),
            (&["tsc", "--noEmit"], "tsc"),
            (&["ruff", "check", "."], "ruff"),
        ];
        for (cmd, expected) in cases {
            let argv: Vec<String> = cmd.iter().map(|s| s.to_string()).collect();
            let hit = registry().find(&command_string(&argv));
            assert_eq!(
                hit.map(|f| f.name.as_str()),
                Some(*expected),
                "command {cmd:?} should route to rule {expected:?}"
            );
        }
    }

    /// JSON/info subcommands must NOT be claimed by the noisy-subcommand rules —
    /// otherwise large JSON would be line-capped instead of structurally
    /// summarized by the generic JSON path.
    #[test]
    fn json_and_info_subcommands_fall_through() {
        let none: &[&[&str]] = &[
            &["go", "list", "-json", "./..."],
            &["go", "env"],
            &["go", "mod", "tidy"],
            &["docker", "ps"],
            &["docker", "inspect", "--format", "json"],
            &["npm", "ls", "--json"],
            &["pnpm", "list", "--json"],
            &["yarn", "info", "react", "--json"],
        ];
        for cmd in none {
            let argv: Vec<String> = cmd.iter().map(|s| s.to_string()).collect();
            let hit = registry().find(&command_string(&argv));
            assert!(
                hit.is_none(),
                "command {cmd:?} should NOT match a rule (got {:?})",
                hit.map(|f| f.name.as_str())
            );
        }
    }

    /// The registry is name-sorted (= rtk's concatenated-BTreeMap order) and has
    /// no two rules whose `match_command` both match the same sample command —
    /// i.e. no silent first-match ambiguity for the common cases.
    #[test]
    fn registry_is_name_sorted_and_unambiguous() {
        let reg = registry();
        let names: Vec<&str> = reg.filters.iter().map(|f| f.name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted, "filters must be globally name-sorted");

        for sample in ["gradle build", "gcc main.c", "make all", "terraform plan", "docker build ."] {
            let n = reg.filters.iter().filter(|f| f.match_regex.is_match(sample)).count();
            assert!(n <= 1, "command {sample:?} matched {n} rules (ambiguous routing)");
        }
    }
}
