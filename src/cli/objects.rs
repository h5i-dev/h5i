//! `h5i objects` — CLI handlers (migrated from main.rs).
use crate::*;

#[derive(Subcommand)]
pub enum ObjectsCommands {
    /// Run a command, store its full raw output out-of-band, and print only a
    /// filtered summary. The exit code is passed through, so this is a
    /// transparent wrapper: `h5i capture run -- pytest -q`.
    Run {
        /// Force a content kind instead of auto-detecting
        /// (test|log|json|diff|generic).
        #[arg(long)]
        kind: Option<String>,
        /// Max lines to keep in the summary.
        #[arg(long)]
        budget: Option<usize>,
        /// Best-effort cap on summary tokens (uses tiktoken when available).
        #[arg(long)]
        token_budget: Option<usize>,
        /// Suppress the durable pointer / status line (print the summary body only).
        #[arg(long)]
        quiet: bool,
        /// Size gate for storing *successful* output: only store + summarize when
        /// it is at least this many bytes; smaller successful output passes
        /// straight through unstored. Failures (nonzero exit) are always stored
        /// regardless of size. Makes it safe to wrap any command. Use 0 to always
        /// capture.
        #[arg(long, default_value_t = DEFAULT_CAPTURE_MIN_BYTES)]
        min_bytes: u64,
        /// Output format: compact (default, one line per finding) | structured
        /// (full YAML) | json | summary (legacy text). Invalid values are rejected.
        #[arg(long, value_enum, default_value_t = CaptureFormat::Compact)]
        format: CaptureFormat,
        /// Associate this capture with a file (repeatable). The branch and the
        /// working-tree diff are recorded automatically.
        #[arg(long = "file", value_name = "PATH", action = clap::ArgAction::Append)]
        files: Vec<String>,
        /// The command to run, after `--`.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },

    /// Store raw bytes read from a file (or stdin with `-`) and print a summary.
    Put {
        /// File to ingest; use `-` to read stdin.
        path: String,
        /// Force a content kind (test|log|json|diff|generic).
        #[arg(long)]
        kind: Option<String>,
        /// Max lines to keep in the summary.
        #[arg(long)]
        budget: Option<usize>,
        /// Associate this capture with a file (repeatable).
        #[arg(long = "file", value_name = "PATH", action = clap::ArgAction::Append)]
        files: Vec<String>,
    },

    /// Rehydrate the full raw bytes for a stored object to stdout.
    /// Accepts a short id, a `sha256:<hex>`, or any unambiguous hex prefix.
    Get {
        /// Object handle (id / sha256:<hex> / prefix).
        id: String,
        /// Print the filtered summary instead of the raw bytes.
        #[arg(long)]
        summary: bool,
        /// Print the full manifest JSON record.
        #[arg(long)]
        manifest: bool,
        /// Re-render the stored structured result — the exact view an agent saw
        /// at capture time — instead of the raw bytes:
        /// compact | structured/yaml | json | summary/text. Takes precedence
        /// over --summary/--manifest.
        #[arg(long, value_enum)]
        format: Option<CaptureFormat>,
    },

    /// List stored objects (most recent first), showing their summaries.
    List {
        /// Maximum number of objects to show.
        #[arg(short, long, default_value_t = 20)]
        limit: usize,
        /// Only objects captured on this branch.
        #[arg(long)]
        branch: Option<String>,
        /// Only objects associated with this file (matches files & diff context).
        #[arg(long)]
        file: Option<String>,
        /// Only objects whose diff context intersects the *current* working-tree
        /// changes — i.e. captures relevant to what you're editing now.
        #[arg(long)]
        diff: bool,
        /// Only objects with this structured status (passed|ok|failed|error|unknown).
        #[arg(long)]
        status: Option<String>,
        /// Only objects from this tool (e.g. pytest, cargo, npm).
        #[arg(long)]
        tool: Option<String>,
        /// Only objects captured inside this environment (`env run`). Accepts the
        /// full id `env/<agent>/<slug>`, `<agent>/<slug>`, or a bare `<slug>`.
        #[arg(long)]
        env: Option<String>,
        /// Emit a structured JSON array (id, cmd, exit, action, tool, status,
        /// env_id, …) instead of the human listing — a typed feed for headless
        /// grading, so a consumer never regex-parses the text.
        #[arg(long)]
        json: bool,
    },

    /// Search captured objects by their normalized findings (and metadata).
    /// Goes deeper than `list`: queries finding message/rule/path/severity/kind
    /// and fingerprints across every captured tool. `--fingerprint` answers
    /// "has this exact failure happened before?".
    Search {
        /// Free-text query (case-insensitive) matched against finding
        /// message/rule/id/detail/location — or the summary for older captures.
        query: Option<String>,
        /// Only findings of this severity (error|warning|failure).
        #[arg(long)]
        severity: Option<String>,
        /// Only findings of this kind (test_failure|diagnostic|build_error|panic|generic).
        #[arg(long)]
        kind: Option<String>,
        /// Only findings whose rule / error code equals this (case-insensitive, e.g. TS2322).
        #[arg(long)]
        rule: Option<String>,
        /// Only findings whose location matches this path fragment (suffix/equality).
        #[arg(long)]
        path: Option<String>,
        /// Only findings whose fingerprint starts with this (recurrence tracking).
        #[arg(long)]
        fingerprint: Option<String>,
        /// Only captures taken on this branch.
        #[arg(long)]
        branch: Option<String>,
        /// Only captures with this structured status (passed|ok|failed|error|unknown).
        #[arg(long)]
        status: Option<String>,
        /// Only captures from this tool (e.g. pytest, cargo, npm).
        #[arg(long)]
        tool: Option<String>,
        /// Only captures taken inside this environment (`env run`). Accepts the
        /// full id `env/<agent>/<slug>`, `<agent>/<slug>`, or a bare `<slug>`.
        #[arg(long)]
        env: Option<String>,
        /// Only captures at most this old (e.g. 7d, 12h, 90m).
        #[arg(long, value_name = "DURATION")]
        since: Option<String>,
        /// Maximum number of matching captures to show.
        #[arg(short, long, default_value_t = 20)]
        limit: usize,
    },

    /// Evict local raw blobs to reclaim space. Manifests/summaries are kept.
    /// Without --ttl, only orphan blobs (no manifest) are removed.
    Gc {
        /// Also evict referenced blobs older than this (e.g. 30d, 12h, 90m).
        #[arg(long, value_name = "DURATION")]
        ttl: Option<String>,
        /// Report what would be evicted without deleting anything.
        #[arg(long)]
        dry_run: bool,
    },

    /// Pin an object so `gc` never evicts its raw blob.
    Pin {
        /// Object handle (id / sha256:<hex> / prefix).
        id: String,
    },

    /// Remove a pin.
    Unpin {
        /// Object handle (id / sha256:<hex> / prefix).
        id: String,
    },

    /// Verify manifests against the local store (absent blobs, orphans).
    Fsck,

    /// Share raw blobs: mirror local raw output into the `refs/h5i/objects-data`
    /// git ref and push it to a remote (the optional git-ref store backend).
    /// Raw output can be large, so this is a deliberate step separate from the
    /// metadata `h5i push`.
    Push {
        /// Remote to push to (default: origin).
        #[arg(long, default_value = "origin")]
        remote: String,
        /// Storage backend: auto (LFS for HTTP(S) remotes, else git-ref) | lfs | git-ref.
        #[arg(long, value_enum, default_value_t = ObjectsBackend::Auto)]
        backend: ObjectsBackend,
    },

    /// Fetch shared raw blobs from a remote (LFS or `refs/h5i/objects-data`) and
    /// cache them into the local store.
    Pull {
        /// Remote to fetch from (default: origin).
        #[arg(long, default_value = "origin")]
        remote: String,
        /// Storage backend: auto (LFS for HTTP(S) remotes, else git-ref) | lfs | git-ref.
        #[arg(long, value_enum, default_value_t = ObjectsBackend::Auto)]
        backend: ObjectsBackend,
    },

    /// List the built-in declarative command filters (the rtk-derived rule set
    /// that `capture run` applies for tools without a coded adapter).
    Filters {
        /// Run every rule's inline golden tests and report pass/fail.
        #[arg(long)]
        verify: bool,
    },

    /// Wire token-reduction guidance into this project's agent instruction files
    /// (.claude/h5i.md, AGENTS.md) so agents know to wrap large-output commands.
    Setup,

    /// Review and trust a project-local `.h5i/filters.toml` so its rules are
    /// applied by `capture run`. Untrusted/changed files are never applied.
    Trust {
        /// Show the current trust status without changing it.
        #[arg(long)]
        status: bool,
        /// Remove trust (project rules will stop being applied).
        #[arg(long)]
        remove: bool,
    },
}

/// Fail-open path for an in-box `capture run` whose h5i side is unusable: run
/// the command unrecorded with inherited stdio and pass its exit code through.
/// A lost capture is acceptable; an agent whose every Bash call dies is not.
fn passthrough_unrecorded(command: &[String], why: &str) -> anyhow::Result<()> {
    if command.is_empty() {
        anyhow::bail!("usage: h5i capture run [--kind K] [--budget N] -- <command> [args…]");
    }
    eprintln!(
        "{} h5i capture unavailable in this box ({why}) — running unrecorded",
        style("warning:").yellow().bold()
    );
    let status = std::process::Command::new(&command[0])
        .args(&command[1..])
        .status()
        .map_err(|e| anyhow::anyhow!("failed to run `{}`: {e}", command.join(" ")))?;
    std::process::exit(status.code().unwrap_or(1));
}

pub fn run(action: ObjectsCommands) -> anyhow::Result<()> {
    {
            use h5i_core::objects::{self, Backend};
            use h5i_core::token_filter::{FilterConfig, OutputKind};

            // Fail OPEN for a wrapped run inside an env box: the wrap-bash hook
            // is unremovable there by design (root-owned managed settings), so a
            // capture-side failure that refused to run the command would brick
            // every Bash call the agent makes. Observation is best-effort — the
            // box's enforcement lives in its mounts/proxy, not in this capture —
            // so degrade to exec-and-passthrough instead. Host-side (out of a
            // box) store errors stay fail-visible: the user can fix or unhook.
            let repo = match H5iRepository::open(".") {
                Ok(r) => r,
                Err(e) => match action {
                    ObjectsCommands::Run { command, .. } if h5i_core::env::in_env_box() => {
                        return passthrough_unrecorded(&command, &e.to_string());
                    }
                    _ => return Err(e.into()),
                },
            };
            let h5i_root = repo.h5i_root.clone();
            let git = repo.git();

            // HEAD tree, recorded on each capture for provenance.
            let head_tree = git
                .head()
                .ok()
                .and_then(|h| h.peel_to_tree().ok())
                .map(|t| t.id().to_string());

            // Build a FilterConfig from the CLI knobs.
            let make_cfg =
                |kind: OutputKind, budget: Option<usize>, token_budget: Option<usize>| {
                    let mut cfg = FilterConfig {
                        kind,
                        token_budget,
                        ..Default::default()
                    };
                    if let Some(b) = budget {
                        cfg.max_lines = b;
                    }
                    cfg
                };

            // Print the agent-facing summary plus a durable pointer line.
            // `quiet` suppresses the pointer/status line (summary only).
            // Prints the durable pointer line (stderr) — the body is printed
            // separately per --format. `quiet` suppresses it.
            let print_pointer = |m: &objects::Manifest, deduped: bool, quiet: bool| {
                if quiet {
                    return;
                }
                // Report tokens for the DEFAULT agent-facing output (compact
                // render when structured), not the git-tracked summary field.
                let savings = match (m.raw_tokens, m.agent_facing_tokens()) {
                    (Some(r), Some(s)) if r > 0 => {
                        let pct = 100 - (s.min(r) * 100 / r);
                        format!(" · ~{pct}% fewer tokens ({r}→{s})")
                    }
                    _ => String::new(),
                };
                eprintln!(
                    "\n{} {} · {} · {} bytes · {} lines{}{}",
                    style("▢ h5i object").dim(),
                    style(&m.id).cyan().bold(),
                    style(&m.kind).yellow(),
                    m.raw_size,
                    m.raw_lines,
                    style(savings).green(),
                    if deduped {
                        style(" · deduped").dim().to_string()
                    } else {
                        String::new()
                    },
                );
                eprintln!(
                    "  {} {}",
                    style("rehydrate:").dim(),
                    style(format!("h5i recall object {}", m.id)).dim(),
                );
            };

            match action {
                ObjectsCommands::Run {
                    kind,
                    budget,
                    token_budget,
                    quiet,
                    min_bytes,
                    format,
                    files,
                    command,
                } => {
                    if command.is_empty() {
                        anyhow::bail!(
                            "usage: h5i capture run [--kind K] [--budget N] -- <command> [args…]"
                        );
                    }
                    let kind_opt = kind
                        .as_deref()
                        .map(OutputKind::parse)
                        .unwrap_or(OutputKind::Auto);
                    let mut cfg = make_cfg(kind_opt, budget, token_budget);
                    // Hand the argv to the filter so command-aware adapters
                    // (pytest/cargo/git) can produce a semantic summary.
                    cfg.cmd = Some(command.clone());
                    // Project-local rules are applied only if the user has
                    // trusted the current `.h5i/filters.toml` (it's untrusted
                    // input that could otherwise mask failures).
                    if let Some(workdir) = git.workdir() {
                        use h5i_core::filter_rules::{self, TrustStatus};
                        match filter_rules::trust_status(workdir, &h5i_root) {
                            TrustStatus::Trusted | TrustStatus::EnvOverride => {
                                // We've decided to apply project rules — make sure
                                // they actually load, rather than silently falling
                                // back to built-ins on a parse error (possible
                                // under H5I_TRUST_FILTERS or a filesystem race).
                                let pf = filter_rules::project_filters_path(workdir);
                                match filter_rules::describe_file(&pf) {
                                    Ok(_) => cfg.project_filters = Some(pf),
                                    Err(e) => eprintln!(
                                        "{} trusted .h5i/filters.toml failed to load — using built-ins only: {e}",
                                        style("warning:").yellow().bold()
                                    ),
                                }
                            }
                            TrustStatus::Untrusted => eprintln!(
                                "{} project .h5i/filters.toml is untrusted — not applied. Review with `h5i objects trust`.",
                                style("warning:").yellow().bold()
                            ),
                            TrustStatus::Changed => eprintln!(
                                "{} .h5i/filters.toml changed since trusted — not applied. Re-review with `h5i objects trust`.",
                                style("warning:").yellow().bold()
                            ),
                            TrustStatus::NoFile => {}
                        }
                    }
                    let cwd = std::env::current_dir()
                        .ok()
                        .map(|p| p.display().to_string());

                    // Run the command, capturing stdout + stderr (stdin inherited
                    // so interactive prompts still work).
                    let output = std::process::Command::new(&command[0])
                        .args(&command[1..])
                        .stdin(std::process::Stdio::inherit())
                        .output();
                    let output = match output {
                        Ok(o) => o,
                        Err(e) => anyhow::bail!("failed to run `{}`: {e}", command.join(" ")),
                    };
                    let exit_code = output.status.code();

                    // Compose the raw payload: stdout, then a labeled stderr block.
                    let mut raw: Vec<u8> =
                        Vec::with_capacity(output.stdout.len() + output.stderr.len() + 32);
                    raw.extend_from_slice(&output.stdout);
                    if !output.stderr.is_empty() {
                        if !raw.is_empty() && !raw.ends_with(b"\n") {
                            raw.push(b'\n');
                        }
                        raw.extend_from_slice(b"\n----- stderr -----\n");
                        raw.extend_from_slice(&output.stderr);
                    }

                    let env_spool = std::env::var_os(h5i_core::env::H5I_ENV_CAPTURE_SPOOL_VAR)
                        .map(PathBuf::from);
                    let env_id = std::env::var(h5i_core::env::H5I_ENV_ID_VAR).ok();
                    let env_policy = std::env::var(h5i_core::env::H5I_ENV_POLICY_DIGEST_VAR).ok();
                    let env_audit_capture =
                        std::env::var(h5i_core::env::H5I_ENV_AUDIT_CAPTURE_VAR).ok();
                    let in_env_capture =
                        env_spool.is_some() && env_id.is_some() && env_policy.is_some();
                    let audit_all = in_env_capture
                        && env_audit_capture
                            .as_deref()
                            .map(|s| s == "all")
                            .unwrap_or(true);

                    // Read-through h5i commands (recall object/objects/search,
                    // team artifact) exist to emit content the caller must read in
                    // FULL. Compacting them defeats the purpose — and in an
                    // audit-all box even a `recall object` rehydrate of a teammate's
                    // diff gets re-summarized. Pass them through verbatim, unstored
                    // (the tee-shim already lets `h5i` commands pass unrecorded), so
                    // an agent never has to reach for `--min-bytes 999999`.
                    let read_through = objects::is_read_through_command(&command);

                    // Signal-aware gate. Store when there's either token-reduction
                    // value (raw ≥ min_bytes) OR provenance/search value (the
                    // command failed). Inside an audit-all h5i env, stage every
                    // wrapped command so env status reflects hook/capture activity
                    // even for small successful commands; the host ingests the spool
                    // later. Legacy envs without the audit var keep the old all-capture
                    // behavior.
                    let worth_storing = !read_through
                        && (audit_all || (raw.len() as u64) >= min_bytes || exit_code != Some(0));
                    if !worth_storing {
                        use std::io::Write;
                        std::io::stdout().write_all(&raw)?;
                    } else if let (Some(spool), Some(_env_id), Some(_env_policy)) =
                        (env_spool, env_id, env_policy)
                    {
                        let meta = h5i_core::env::InboxCaptureMeta {
                            cmd: command.join(" "),
                            cwd: cwd.clone(),
                            exit_code,
                            files: files.clone(),
                            cmd_argv: command.clone(),
                        };
                        // The command already ran — a spool failure must never
                        // discard its output or fabricate a failing exit code.
                        // Fail open: emit the raw output unrecorded and fall
                        // through to the exit-code passthrough below.
                        match h5i_core::env::write_inbox_capture_spool(&spool, &meta, &raw) {
                            Ok(staged) => {
                                let text = String::from_utf8_lossy(&raw);
                                let filtered = h5i_core::token_filter::filter(&text, &cfg);
                                let structured =
                                    h5i_core::structured::parse(&command, &text, exit_code);
                                match (format, &structured) {
                                    (CaptureFormat::Summary | CaptureFormat::Text, _)
                                    | (_, None) => {
                                        println!("{}", filtered.summary)
                                    }
                                    (CaptureFormat::Json, Some(s)) => {
                                        println!("{}", h5i_core::structured::render_json_pretty(s))
                                    }
                                    (CaptureFormat::Structured | CaptureFormat::Yaml, Some(s)) => {
                                        println!("{}", h5i_core::structured::render_yaml(s))
                                    }
                                    (CaptureFormat::Compact, Some(s)) => {
                                        println!("{}", h5i_core::structured::render_compact(s))
                                    }
                                }
                                if !quiet {
                                    eprintln!(
                                        "\n{} {} · inbox-capture · staged for host ingest",
                                        style("▢ h5i env capture").dim(),
                                        style(staged).cyan().bold(),
                                    );
                                }
                            }
                            Err(e) => {
                                eprintln!(
                                    "{} env capture spool write failed ({e}) — output passed \
                                     through unrecorded",
                                    style("warning:").yellow().bold()
                                );
                                use std::io::Write;
                                std::io::stdout().write_all(&raw)?;
                            }
                        }
                    } else {
                        let opts = objects::CaptureOptions {
                            kind: kind_opt,
                            cmd: Some(command.join(" ")),
                            cwd,
                            exit_code,
                            git_tree: head_tree.clone(),
                            files,
                            cmd_argv: command.clone(),
                            filter: cfg,
                            env_id: None,
                            policy_digest: None,
                            evidence_source: None,
                            egress: None,
                            redact: false,
                        };
                        let outcome = objects::capture(git, &h5i_root, &raw, opts)?;
                        let m = &outcome.manifest;
                        // Render the body per --format (compact is the default).
                        // Falls back to the text summary if no structured record.
                        match (format, &m.structured) {
                            (CaptureFormat::Summary | CaptureFormat::Text, _) | (_, None) => {
                                println!("{}", m.summary)
                            }
                            (CaptureFormat::Json, Some(s)) => {
                                println!("{}", h5i_core::structured::render_json_pretty(s))
                            }
                            (CaptureFormat::Structured | CaptureFormat::Yaml, Some(s)) => {
                                println!("{}", h5i_core::structured::render_yaml(s))
                            }
                            (CaptureFormat::Compact, Some(s)) => {
                                println!("{}", h5i_core::structured::render_compact(s))
                            }
                        }
                        print_pointer(m, outcome.deduped, quiet);
                    }

                    // Transparent wrapper: pass the child's exit code through.
                    if let Some(code) = exit_code {
                        if code != 0 {
                            std::process::exit(code);
                        }
                    }
                }

                ObjectsCommands::Put {
                    path,
                    kind,
                    budget,
                    files,
                } => {
                    let raw = if path == "-" {
                        use std::io::Read;
                        let mut buf = Vec::new();
                        std::io::stdin().read_to_end(&mut buf)?;
                        buf
                    } else {
                        std::fs::read(&path).map_err(|e| anyhow::anyhow!("read {path}: {e}"))?
                    };
                    let kind_opt = kind
                        .as_deref()
                        .map(OutputKind::parse)
                        .unwrap_or(OutputKind::Auto);
                    let cfg = make_cfg(kind_opt, budget, None);
                    let opts = objects::CaptureOptions {
                        kind: kind_opt,
                        cmd: None,
                        cwd: None,
                        exit_code: None,
                        git_tree: head_tree.clone(),
                        files,
                        cmd_argv: Vec::new(),
                        filter: cfg,
                        env_id: None,
                        policy_digest: None,
                        evidence_source: None,
                        egress: None,
                        redact: false,
                    };
                    let outcome = objects::capture(git, &h5i_root, &raw, opts)?;
                    println!("{}", outcome.manifest.summary);
                    print_pointer(&outcome.manifest, outcome.deduped, false);
                }

                ObjectsCommands::Get {
                    id,
                    summary,
                    manifest,
                    format,
                } => {
                    let m = match objects::resolve_manifest(git, &id) {
                        Ok(m) => m,
                        Err(e) => {
                            // In a box, a capture the agent just made may still be
                            // STAGED in the spool (not yet ingested into
                            // refs/h5i/objects). Rehydrate its full raw output from
                            // the spool so the agent can read what `capture run`
                            // compacted — instead of a misleading "no object matches".
                            if let Some(staged) = h5i_core::env::read_staged_capture(&id) {
                                if manifest || format.is_some() {
                                    anyhow::bail!(
                                        "capture {id} is staged in-box and not yet ingested — the \
                                         manifest / structured views are computed by the host on \
                                         session end. Its raw output is available now: \
                                         h5i recall object {id}"
                                    );
                                }
                                if summary {
                                    println!("staged capture {id} (not yet ingested by the host)");
                                    if let Some(meta) = &staged.meta {
                                        if !meta.cmd.is_empty() {
                                            println!("  cmd:  {}", meta.cmd);
                                        }
                                        if let Some(code) = meta.exit_code {
                                            println!("  exit: {code}");
                                        }
                                    }
                                    println!("  raw:  h5i recall object {id}");
                                } else {
                                    use std::io::Write;
                                    std::io::stdout().write_all(&staged.raw)?;
                                }
                                return Ok(());
                            }
                            return Err(e.into());
                        }
                    };
                    if let Some(fmt) = format {
                        // Re-render the stored structured result exactly as it was
                        // shown at capture time. The summary/text formats fall back
                        // to the free-text summary (always present); the structured
                        // formats need the structured record.
                        use h5i_core::structured as st;
                        let need_structured =
                            !matches!(fmt, CaptureFormat::Summary | CaptureFormat::Text);
                        if need_structured && m.structured.is_none() {
                            anyhow::bail!(
                                "object {} has no structured result to render as {:?} \
                                 (older or non-command capture). Use --summary for its text, \
                                 or `h5i recall object {} --manifest` for the raw record.",
                                m.id,
                                fmt,
                                m.id
                            );
                        }
                        match (fmt, m.structured.as_ref()) {
                            (CaptureFormat::Compact, Some(s)) => {
                                println!("{}", st::render_compact(s))
                            }
                            (CaptureFormat::Structured | CaptureFormat::Yaml, Some(s)) => {
                                println!("{}", st::render_yaml(s))
                            }
                            (CaptureFormat::Json, Some(s)) => {
                                println!("{}", st::render_json_pretty(s))
                            }
                            // Summary/Text (and the unreachable None arms guarded above).
                            _ => println!("{}", m.summary),
                        }
                    } else if manifest {
                        println!("{}", serde_json::to_string_pretty(&m)?);
                    } else if summary {
                        println!("{}", m.summary);
                    } else {
                        // Local first, then the git-ref store (shared blobs).
                        match objects::load_raw_with_remote(git, &h5i_root, &m)? {
                            Some(bytes) => {
                                use std::io::Write;
                                std::io::stdout().write_all(&bytes)?;
                            }
                            None => anyhow::bail!(
                                "raw blob for {} is absent (evicted, or never fetched).\n\
                                 Its summary is still available: h5i recall object {} --summary\n\
                                 If it was shared, run `h5i objects pull` (for an LFS/HTTP(S) \
                                 remote, check the remote/credentials). Note: lazy recall only \
                                 tries the `origin` remote.",
                                m.raw_oid,
                                m.id
                            ),
                        }
                    }
                }

                ObjectsCommands::List {
                    limit,
                    branch,
                    file,
                    diff,
                    status,
                    tool,
                    env,
                    json,
                } => {
                    let all = objects::read_manifests(git);

                    // Validate --status against the canonical vocabulary (the
                    // structured status enum), case-insensitively.
                    let status = match status {
                        Some(s) => {
                            let sl = s.to_lowercase();
                            const VALID: &[&str] = &["passed", "ok", "failed", "error", "unknown"];
                            if !VALID.contains(&sl.as_str()) {
                                anyhow::bail!(
                                    "invalid --status '{s}' (expected one of: {})",
                                    VALID.join(", ")
                                );
                            }
                            Some(sl)
                        }
                        None => None,
                    };

                    // Build the optional filters.
                    let cur_diff: Vec<String> = if diff {
                        objects::working_diff_files(git)
                    } else {
                        Vec::new()
                    };
                    let file_matches = |m: &objects::Manifest, needle: &str| {
                        m.files.iter().chain(m.diff_files.iter()).any(|f| {
                            f == needle || f.ends_with(needle) || needle.ends_with(f.as_str())
                        })
                    };
                    let manifests: Vec<&objects::Manifest> = all
                        .iter()
                        .filter(|m| {
                            branch
                                .as_deref()
                                .is_none_or(|b| m.branch.as_deref() == Some(b))
                        })
                        .filter(|m| file.as_deref().is_none_or(|f| file_matches(m, f)))
                        .filter(|m| {
                            !diff
                                || m.files
                                    .iter()
                                    .chain(m.diff_files.iter())
                                    .any(|f| cur_diff.iter().any(|c| c == f))
                        })
                        .filter(|m| {
                            status.as_deref().is_none_or(|want| {
                                m.structured.as_ref().is_some_and(|s| {
                                    serde_json::to_value(s.status)
                                        .ok()
                                        .and_then(|v| v.as_str().map(str::to_string))
                                        .as_deref()
                                        == Some(want)
                                })
                            })
                        })
                        .filter(|m| {
                            tool.as_deref().is_none_or(|want| {
                                m.structured.as_ref().map(|s| s.tool.as_str()) == Some(want)
                            })
                        })
                        .filter(|m| {
                            env.as_deref().is_none_or(|want| {
                                objects::env_id_matches(m.env_id.as_deref(), want)
                            })
                        })
                        .collect();

                    if json {
                        // Typed feed for headless grading: newest first, capped at
                        // --limit. Structured fields lifted from the manifest +
                        // structured result so a consumer never parses text.
                        let store = objects::LocalStore::new(&h5i_root);
                        #[derive(serde::Serialize)]
                        struct ObjJson<'a> {
                            id: &'a str,
                            timestamp: &'a str,
                            #[serde(skip_serializing_if = "Option::is_none")]
                            cmd: Option<&'a str>,
                            #[serde(skip_serializing_if = "Option::is_none")]
                            exit_code: Option<i32>,
                            /// Action class (test|build|read|write|egress|other).
                            #[serde(skip_serializing_if = "Option::is_none")]
                            action: Option<&'a str>,
                            /// Program adapter (pytest|cargo|…), from the structured result.
                            #[serde(skip_serializing_if = "Option::is_none")]
                            tool: Option<&'a str>,
                            /// Validated pass/fail (passed|failed|error|…), when known.
                            #[serde(skip_serializing_if = "Option::is_none")]
                            status: Option<String>,
                            #[serde(skip_serializing_if = "Option::is_none")]
                            duration_ms: Option<u64>,
                            kind: &'a str,
                            #[serde(skip_serializing_if = "Option::is_none")]
                            branch: Option<&'a str>,
                            #[serde(skip_serializing_if = "Option::is_none")]
                            env_id: Option<&'a str>,
                            /// Authoritative egress verdicts (container proxy /
                            /// supervised socket-gate): allowed/denied counts +
                            /// per-host. `denied > 0` is an *enforced* boundary
                            /// block — not inferred from an exit code. Absent on
                            /// tiers with no egress enforcement.
                            #[serde(skip_serializing_if = "Option::is_none")]
                            egress: Option<&'a h5i_core::sandbox_policy::EgressSummary>,
                            raw_size: u64,
                            raw_present: bool,
                        }
                        let rows: Vec<ObjJson> = manifests
                            .iter()
                            .rev()
                            .take(limit)
                            .map(|m| {
                                let st = m.structured.as_ref();
                                ObjJson {
                                    id: &m.id,
                                    timestamp: &m.timestamp,
                                    cmd: m.cmd.as_deref(),
                                    exit_code: m.exit_code,
                                    action: m.action.as_deref(),
                                    tool: st.map(|s| s.tool.as_str()),
                                    status: st.and_then(|s| {
                                        serde_json::to_value(s.status)
                                            .ok()
                                            .and_then(|v| v.as_str().map(str::to_string))
                                    }),
                                    duration_ms: st.and_then(|s| s.duration_ms),
                                    kind: &m.kind,
                                    branch: m.branch.as_deref(),
                                    env_id: m.env_id.as_deref(),
                                    egress: m.egress.as_ref(),
                                    raw_size: m.raw_size,
                                    raw_present: store.has(m.hex()),
                                }
                            })
                            .collect();
                        println!("{}", serde_json::to_string_pretty(&rows)?);
                        return Ok(());
                    }

                    let filtered = branch.is_some()
                        || file.is_some()
                        || diff
                        || status.is_some()
                        || tool.is_some()
                        || env.is_some();
                    if manifests.is_empty() {
                        if filtered {
                            println!("No captured objects match that filter.");
                        } else {
                            println!(
                                "No captured objects yet. Try: {}",
                                style("h5i capture run -- <command>").bold()
                            );
                        }
                    } else {
                        let store = objects::LocalStore::new(&h5i_root);
                        let total = manifests.len();
                        println!(
                            "{} object{}{} (newest first){}\n",
                            total,
                            if total == 1 { "" } else { "s" },
                            if filtered { " matched" } else { " captured" },
                            if total > limit {
                                format!(" — showing {limit}")
                            } else {
                                String::new()
                            }
                        );
                        for m in manifests.iter().rev().take(limit) {
                            let present = store.has(m.hex());
                            let dot = if present {
                                style("●").green()
                            } else {
                                style("○").red()
                            };
                            let first_line = m.summary.lines().next().unwrap_or("").trim();
                            let branch_tag = m
                                .branch
                                .as_deref()
                                .map(|b| format!("  ⎇ {b}"))
                                .unwrap_or_default();
                            println!(
                                "{} {}  {}  {} bytes · {} lines{}",
                                dot,
                                style(&m.id).cyan().bold(),
                                style(&m.kind).yellow(),
                                m.raw_size,
                                m.raw_lines,
                                style(branch_tag).magenta()
                            );
                            if let Some(cmd) = &m.cmd {
                                println!("    {} {}", style("$").dim(), style(cmd).dim());
                            }
                            // Show the files this capture is about (subject ∪ diff).
                            let mut shown: Vec<&String> =
                                m.files.iter().chain(m.diff_files.iter()).collect();
                            shown.sort();
                            shown.dedup();
                            if !shown.is_empty() {
                                let preview: Vec<&str> =
                                    shown.iter().take(4).map(|s| s.as_str()).collect();
                                let more = shown.len().saturating_sub(4);
                                let extra = if more > 0 {
                                    format!(" +{more}")
                                } else {
                                    String::new()
                                };
                                println!(
                                    "    {} {}{}",
                                    style("⊞").dim(),
                                    style(preview.join(", ")).dim(),
                                    style(extra).dim()
                                );
                            }
                            println!("    {}", style(first_line).dim());
                        }
                        println!(
                            "\n{} = raw present locally · {} = absent (rehydrate from a remote)",
                            style("●").green(),
                            style("○").red()
                        );
                    }
                }

                ObjectsCommands::Search {
                    query,
                    severity,
                    kind,
                    rule,
                    path,
                    fingerprint,
                    branch,
                    status,
                    tool,
                    env,
                    since,
                    limit,
                } => {
                    // Validate enum-valued filters up front against the canonical
                    // vocabularies, case-insensitively (mirrors `list --status`).
                    let validate = |val: Option<String>,
                                    name: &str,
                                    valid: &[&str]|
                     -> anyhow::Result<Option<String>> {
                        match val {
                            Some(s) => {
                                let sl = s.to_lowercase();
                                if !valid.contains(&sl.as_str()) {
                                    anyhow::bail!(
                                        "invalid --{name} '{s}' (expected one of: {})",
                                        valid.join(", ")
                                    );
                                }
                                Ok(Some(sl))
                            }
                            None => Ok(None),
                        }
                    };
                    let severity =
                        validate(severity, "severity", &["error", "warning", "failure"])?;
                    let kind = validate(
                        kind,
                        "kind",
                        &[
                            "test_failure",
                            "diagnostic",
                            "build_error",
                            "panic",
                            "generic",
                        ],
                    )?;
                    let status = validate(
                        status,
                        "status",
                        &["passed", "ok", "failed", "error", "unknown"],
                    )?;

                    // `--since 7d` → an absolute RFC3339 cutoff in the manifest's
                    // timestamp format, so the pure matcher only does a lexical compare.
                    let since = match since {
                        Some(s) => {
                            let dur = objects::parse_duration(&s)?;
                            let cutoff = chrono::Utc::now()
                                - chrono::Duration::from_std(dur)
                                    .map_err(|e| anyhow::anyhow!("duration too large: {e}"))?;
                            Some(cutoff.format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string())
                        }
                        None => None,
                    };

                    let filters = objects::SearchFilters {
                        query: query.clone(),
                        severity,
                        kind,
                        rule: rule.clone(),
                        path: path.clone(),
                        fingerprint: fingerprint.clone(),
                        branch: branch.clone(),
                        status,
                        tool: tool.clone(),
                        env: env.clone(),
                        since,
                    };

                    let all = objects::read_manifests(git);
                    // read_manifests is oldest-first; search preserves order, so
                    // reverse to newest-first for display.
                    let newest: Vec<objects::Manifest> = all.into_iter().rev().collect();
                    let hits = objects::search_manifests(&newest, &filters);

                    if hits.is_empty() {
                        println!("No captured findings match that search.");
                    } else {
                        let store = objects::LocalStore::new(&h5i_root);
                        let total = hits.len();
                        let total_findings: usize = hits.iter().map(|h| h.findings.len()).sum();
                        println!(
                            "{} capture{} matched · {} finding{} (newest first){}\n",
                            total,
                            if total == 1 { "" } else { "s" },
                            total_findings,
                            if total_findings == 1 { "" } else { "s" },
                            if total > limit {
                                format!(" — showing {limit}")
                            } else {
                                String::new()
                            }
                        );
                        for hit in hits.iter().take(limit) {
                            let m = hit.manifest;
                            let present = store.has(m.hex());
                            let dot = if present {
                                style("●").green()
                            } else {
                                style("○").red()
                            };
                            let tool_tag = m
                                .structured
                                .as_ref()
                                .map(|s| s.tool.clone())
                                .unwrap_or_else(|| m.kind.clone());
                            let branch_tag = m
                                .branch
                                .as_deref()
                                .map(|b| format!("  ⎇ {b}"))
                                .unwrap_or_default();
                            println!(
                                "{} {}  {}{}",
                                dot,
                                style(&m.id).cyan().bold(),
                                style(tool_tag).yellow(),
                                style(branch_tag).magenta()
                            );
                            if let Some(cmd) = &m.cmd {
                                println!("    {} {}", style("$").dim(), style(cmd).dim());
                            }
                            // Cap findings shown per capture to stay token-light;
                            // the full set is one `recall object <id>` away.
                            const PER_CAPTURE: usize = 8;
                            for f in hit.findings.iter().take(PER_CAPTURE) {
                                let loc = f
                                    .location
                                    .as_ref()
                                    .map(|l| l.shorthand())
                                    .unwrap_or_default();
                                let rule = f
                                    .rule
                                    .as_deref()
                                    .map(|r| format!("[{r}] "))
                                    .unwrap_or_default();
                                let sev = objects_severity_label(&f.severity);
                                let msg = f.message.lines().next().unwrap_or("").trim();
                                let msg = truncate(msg, 100);
                                if loc.is_empty() {
                                    println!("    {sev} {}{}", style(rule).dim(), msg);
                                } else {
                                    println!(
                                        "    {sev} {}{}  {}",
                                        style(rule).dim(),
                                        msg,
                                        style(loc).blue()
                                    );
                                }
                            }
                            let more = hit.findings.len().saturating_sub(PER_CAPTURE);
                            if more > 0 {
                                println!(
                                    "    {}",
                                    style(format!("… +{more} more finding(s)")).dim()
                                );
                            }
                            if hit.findings.is_empty() {
                                // Capture-level (textual/metadata) match — show the summary head.
                                let first = m.summary.lines().next().unwrap_or("").trim();
                                if !first.is_empty() {
                                    println!("    {}", style(first).dim());
                                }
                            }
                        }
                        println!(
                            "\nRehydrate full output with {}",
                            style("h5i recall object <id>").bold()
                        );
                    }
                }

                ObjectsCommands::Gc { ttl, dry_run } => {
                    let ttl = match ttl {
                        Some(s) => Some(objects::parse_duration(&s)?),
                        None => None,
                    };
                    let report = objects::gc(git, &h5i_root, ttl, dry_run)?;
                    let verb = if report.dry_run {
                        "would evict"
                    } else {
                        "evicted"
                    };
                    println!(
                        "{} {} blob{} ({} freed) · kept {} referenced, {} pinned · {} total",
                        if report.dry_run {
                            style("DRY RUN:").yellow().bold()
                        } else {
                            style("GC:").green().bold()
                        },
                        report.evicted.len(),
                        if report.evicted.len() == 1 { "" } else { "s" },
                        humanize_bytes(report.freed_bytes),
                        report.kept_referenced,
                        report.kept_pinned,
                        report.total_blobs,
                    );
                    for e in report.evicted.iter().take(50) {
                        println!(
                            "  {} {}  {} bytes  ({})",
                            style(verb).dim(),
                            style(&e.hex[..16.min(e.hex.len())]).dim(),
                            e.size,
                            e.reason
                        );
                    }
                }

                ObjectsCommands::Pin { id } => {
                    let m = objects::resolve_manifest(git, &id)?;
                    objects::pin(&h5i_root, m.hex())?;
                    println!("{} pinned {}", style("✔").green(), style(&m.id).cyan());
                }

                ObjectsCommands::Unpin { id } => {
                    let m = objects::resolve_manifest(git, &id)?;
                    objects::unpin(&h5i_root, m.hex())?;
                    println!("{} unpinned {}", style("✔").green(), style(&m.id).cyan());
                }

                ObjectsCommands::Filters { verify } => {
                    if verify {
                        let (passed, failures) = h5i_core::filter_rules::run_golden_tests();
                        if failures.is_empty() {
                            println!(
                                "{} all {} golden test(s) passed across {} rules",
                                style("✔").green(),
                                passed,
                                h5i_core::filter_rules::list_filters().len()
                            );
                        } else {
                            println!(
                                "{} {} passed, {} failed",
                                style("✗").red(),
                                passed,
                                failures.len()
                            );
                            for f in failures.iter().take(20) {
                                println!("  {} {}/{}", style("✗").red(), f.filter, f.test);
                            }
                            std::process::exit(1);
                        }
                    } else {
                        let rules = h5i_core::filter_rules::list_filters();
                        println!(
                            "{} built-in command filters (rtk-derived; applied by `h5i capture run`)\n",
                            rules.len()
                        );
                        let w = rules.iter().map(|(n, _, _)| n.len()).max().unwrap_or(0);
                        for (name, desc, _pat) in &rules {
                            println!(
                                "  {:<w$}  {}",
                                style(name).cyan().bold(),
                                style(desc).dim(),
                                w = w
                            );
                        }
                        println!(
                            "\n{} coded adapters (pytest, cargo, git diff) take precedence; \
                             then these rules; then the generic scorer.",
                            style("note:").dim()
                        );
                    }
                }

                ObjectsCommands::Setup => {
                    let workdir = git
                        .workdir()
                        .ok_or_else(|| anyhow::anyhow!("bare repository not supported"))?;
                    let mut wrote = Vec::new();
                    let mut skipped = Vec::new();

                    // Always ensure .claude/h5i.md carries the guidance.
                    let h5i_md = workdir.join(".claude").join("h5i.md");
                    if append_block_if_missing(
                        &h5i_md,
                        CAPTURE_GUIDANCE_MARKER,
                        CAPTURE_GUIDANCE_BLOCK,
                    )? {
                        wrote.push(".claude/h5i.md");
                    } else {
                        skipped.push(".claude/h5i.md");
                    }
                    // Update AGENTS.md only if the project already uses one.
                    let agents = workdir.join("AGENTS.md");
                    if agents.exists() {
                        if append_block_if_missing(
                            &agents,
                            CAPTURE_GUIDANCE_MARKER,
                            CAPTURE_GUIDANCE_BLOCK,
                        )? {
                            wrote.push("AGENTS.md");
                        } else {
                            skipped.push("AGENTS.md");
                        }
                    }

                    if wrote.is_empty() {
                        println!(
                            "{} capture guidance already present in {}",
                            style("✓").green(),
                            skipped.join(", ")
                        );
                    } else {
                        println!(
                            "{} wired token-reduction guidance into {}",
                            style("✔").green(),
                            wrote.join(", ")
                        );
                        if !skipped.is_empty() {
                            println!("  (already present in {})", skipped.join(", "));
                        }
                    }
                    println!(
                        "\nAgents will now wrap large-output commands with {}.",
                        style("h5i capture run").bold()
                    );
                }

                ObjectsCommands::Trust { status, remove } => {
                    use h5i_core::filter_rules::{self, TrustStatus};
                    let workdir = git
                        .workdir()
                        .ok_or_else(|| anyhow::anyhow!("bare repository not supported"))?;
                    let path = filter_rules::project_filters_path(workdir);
                    let st = filter_rules::trust_status(workdir, &h5i_root);

                    if remove {
                        filter_rules::untrust(&h5i_root).map_err(|e| anyhow::anyhow!(e))?;
                        println!("{} project filters untrusted", style("✔").green());
                    } else if status {
                        let label = match st {
                            TrustStatus::NoFile => "no .h5i/filters.toml present",
                            TrustStatus::Untrusted => "present, NOT trusted",
                            TrustStatus::Changed => "changed since trusted (re-review)",
                            TrustStatus::Trusted => "trusted",
                            TrustStatus::EnvOverride => "applied via H5I_TRUST_FILTERS override",
                        };
                        println!("{}  ({})", style(label).bold(), path.display());
                    } else {
                        if st == TrustStatus::NoFile {
                            anyhow::bail!("no {} to trust — create it first", path.display());
                        }
                        // Review: show the rules and flag any that could hide output.
                        let rules =
                            filter_rules::describe_file(&path).map_err(|e| anyhow::anyhow!(e))?;
                        println!(
                            "Reviewing {} ({} rule{}):\n",
                            style(path.display()).bold(),
                            rules.len(),
                            if rules.len() == 1 { "" } else { "s" }
                        );
                        let mut risky = false;
                        for r in &rules {
                            let flag = if r.can_hide_output {
                                risky = true;
                                style(" ⚠ can short-circuit output").red().to_string()
                            } else {
                                String::new()
                            };
                            println!(
                                "  {}  {}{}",
                                style(&r.name).cyan().bold(),
                                style(&r.match_pattern).dim(),
                                flag
                            );
                        }
                        if risky {
                            println!(
                                "\n{} one or more rules use match_output without an `unless` guard — they can replace real output with a fixed message.",
                                style("note:").yellow().bold()
                            );
                        }
                        let hash = filter_rules::trust(workdir, &h5i_root)
                            .map_err(|e| anyhow::anyhow!(e))?;
                        println!(
                            "\n{} trusted {} (sha256:{})",
                            style("✔").green(),
                            path.display(),
                            &hash[..12]
                        );
                    }
                }

                ObjectsCommands::Fsck => {
                    let report = objects::fsck(git, &h5i_root)?;
                    println!(
                        "{} manifests · {} absent · {} orphan blob{}",
                        report.rows.len(),
                        report.absent,
                        report.orphans.len(),
                        if report.orphans.len() == 1 { "" } else { "s" }
                    );
                    for row in report.rows.iter().filter(|r| !r.present) {
                        println!(
                            "  {} {} absent{}",
                            style("✗").red(),
                            style(&row.id).cyan(),
                            if row.pinned { " (pinned!)" } else { "" }
                        );
                    }
                    if !report.orphans.is_empty() {
                        println!(
                            "  {} run `h5i objects gc` to remove orphan blobs",
                            style("tip:").dim()
                        );
                    }
                }

                ObjectsCommands::Push { remote, backend } => {
                    let workdir = git.workdir().ok_or_else(|| {
                        anyhow::anyhow!("h5i objects push requires a working tree")
                    })?;
                    let url = remote_url(git, &remote);
                    let use_lfs = match backend {
                        ObjectsBackend::GitRef => false,
                        ObjectsBackend::Lfs => true,
                        ObjectsBackend::Auto => url
                            .as_deref()
                            .and_then(h5i_core::lfs::endpoint_for_remote)
                            .is_some(),
                    };
                    if use_lfs {
                        let u = url
                            .clone()
                            .ok_or_else(|| anyhow::anyhow!("remote '{remote}' has no URL"))?;
                        match lfs_push(git, workdir, &h5i_root, &u) {
                            Ok(n) => println!(
                                "{} {} blob{} uploaded to LFS on {}",
                                style("✔").green(),
                                n,
                                if n == 1 { "" } else { "s" },
                                style(&remote).cyan()
                            ),
                            // Auto falls back to the git-ref store ONLY when the
                            // remote clearly lacks LFS — never on auth/network/
                            // content failures (those must surface).
                            Err(e) if backend == ObjectsBackend::Auto && e.is_unsupported() => {
                                eprintln!(
                                    "  {} {}; falling back to the git-ref store",
                                    style("ℹ").dim(),
                                    e
                                );
                                git_ref_push(git, workdir, &h5i_root, &remote)?;
                            }
                            Err(e) => return Err(e.into()),
                        }
                    } else {
                        git_ref_push(git, workdir, &h5i_root, &remote)?;
                    }
                }

                ObjectsCommands::Pull { remote, backend } => {
                    let workdir = git.workdir().ok_or_else(|| {
                        anyhow::anyhow!("h5i objects pull requires a working tree")
                    })?;
                    let url = remote_url(git, &remote);
                    let use_lfs = match backend {
                        ObjectsBackend::GitRef => false,
                        ObjectsBackend::Lfs => true,
                        ObjectsBackend::Auto => url
                            .as_deref()
                            .and_then(h5i_core::lfs::endpoint_for_remote)
                            .is_some(),
                    };
                    if use_lfs {
                        let u = url
                            .clone()
                            .ok_or_else(|| anyhow::anyhow!("remote '{remote}' has no URL"))?;
                        match lfs_pull(git, workdir, &h5i_root, &u) {
                            Ok((got, missing)) => println!(
                                "{} {} blob{} fetched from LFS · cached locally{}",
                                style("✔").green(),
                                got,
                                if got == 1 { "" } else { "s" },
                                if missing > 0 {
                                    format!(" · {missing} not available on the server")
                                } else {
                                    String::new()
                                }
                            ),
                            Err(e) if backend == ObjectsBackend::Auto && e.is_unsupported() => {
                                eprintln!(
                                    "  {} {}; falling back to the git-ref store",
                                    style("ℹ").dim(),
                                    e
                                );
                                git_ref_pull(git, workdir, &h5i_root, &remote)?;
                            }
                            Err(e) => return Err(e.into()),
                        }
                    } else {
                        git_ref_pull(git, workdir, &h5i_root, &remote)?;
                    }
                }
            }
        }
    Ok(())
}
