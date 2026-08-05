//! `h5i` — a disposable, confined development environment for coding agents.
//!
//! The binary is a thin router: every noun's clap enum and handlers live in
//! `cli/`, and `main` does argument bootstrap plus dispatch.

use clap::{CommandFactory, Parser, Subcommand};

mod cli;

#[derive(Parser)]
#[command(
    name = "h5i",
    about = "Disposable, confined development environments for coding agents",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
// `Dev`'s clap enum is much larger than the two generator commands; boxing it
// would break clap's derive, and the enum is constructed once per process.
#[allow(clippy::large_enum_variant)]
enum Commands {
    /// Disposable, confined development boxes. `h5i dev` with no verb creates
    /// one from the current repository; `h5i dev --help` has the verb table.
    Dev(DevArgs),

    /// Deprecated alias for `h5i dev`. Kept for one release.
    #[command(hide = true)]
    Env {
        #[command(subcommand)]
        action: cli::dev::DevCommands,
    },

    /// The browser control lock: who is driving the browser in a box.
    Browser {
        #[command(subcommand)]
        action: cli::browser::BrowserCommands,
    },

    /// Write or print the agent skill this binary carries.
    Skill {
        #[command(subcommand)]
        action: cli::skill::SkillCommands,
    },

    /// Generate a shell completion script (bash, zsh, fish, …); e.g.
    /// `h5i completion bash > /etc/bash_completion.d/h5i`
    Completion {
        /// Shell to generate completions for
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },

    /// Generate the roff man page from the CLI definition (so it never drifts
    /// from the actual commands); e.g. `h5i man > man/man1/h5i.1`
    Man,
}

/// `h5i dev` with no verb is "make me a box from this source". With a verb it
/// is the lifecycle surface. clap resolves the verb first, so a source that
/// happens to be spelled like a verb needs the explicit `h5i dev create` form.
#[derive(clap::Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct DevArgs {
    #[command(subcommand)]
    action: Option<cli::dev::DevCommands>,

    /// Where the code comes from: `.` for this repository (the default), a
    /// GitHub pull request (number, #number, or URL), or a repository URL to
    /// copy into the box.
    #[arg(value_name = "SOURCE")]
    source: Option<String>,

    /// Start from an empty box instead of any source.
    #[arg(long, conflicts_with = "source")]
    new: bool,

    /// Name for the box. Derived from the source when omitted.
    #[arg(long)]
    name: Option<String>,

    /// Base revision, when the source is this repository. Pinned immutably.
    #[arg(long)]
    from: Option<String>,

    /// Remote to fetch a pull request head from.
    #[arg(long, default_value = "origin")]
    remote: String,

    /// Policy profile from .h5i/env.toml (see `h5i dev create --help`).
    #[arg(long)]
    profile: Option<String>,

    /// Isolation tier: auto (default) | workspace | process | supervised | container | ...
    #[arg(long)]
    isolation: Option<String>,

    /// Container base image for isolation=container.
    #[arg(long)]
    image: Option<String>,
}

impl DevArgs {
    /// Fold the short form into the same `Create` the explicit verb builds, so
    /// there is exactly one create path to reason about.
    fn into_command(self) -> anyhow::Result<cli::dev::DevCommands> {
        if let Some(action) = self.action {
            return Ok(action);
        }
        let source = self.source.unwrap_or_else(|| ".".to_string());
        let (pr, clone) = if self.new {
            (None, None)
        } else {
            match cli::dev::pr_spec(&source) {
                Some(spec) => (Some(spec), None),
                None if source == "." => (None, None),
                None if cli::dev::looks_like_repo_url(&source) => (None, Some(source.clone())),
                None => anyhow::bail!(
                    "unrecognized source '{source}'.\n  \
                     Pass `.` for this repository, a pull request (number, #number, or URL), \
                     a repository URL, or --new for an empty box."
                ),
            }
        };
        let name = match (self.name, &pr, &clone) {
            (Some(n), _, _) => n,
            (None, Some(spec), _) => format!("pr-{spec}"),
            (None, None, Some(url)) => cli::dev::name_from_url(url)?,
            (None, None, None) if self.new => cli::dev::free_box_name("new")?,
            (None, None, None) => cli::dev::default_box_name()?,
        };
        Ok(cli::dev::DevCommands::Create {
            name,
            from: self.from,
            pr,
            clone,
            new: self.new,
            remote: self.remote,
            profile: self.profile,
            isolation: self.isolation,
            image: self.image,
            backend: "auto".into(),
            audit: "signal".into(),
        })
    }
}

fn main() -> anyhow::Result<()> {
    init_tracing();
    let argv: Vec<String> = std::env::args().collect();
    maybe_version_json(&argv);
    let cli = Cli::parse_from(argv);

    match cli.command {
        Commands::Dev(args) => cli::dev::run(args.into_command()?)?,
        Commands::Env { action } => cli::dev::run(action)?,
        Commands::Browser { action } => cli::browser::run(action)?,
        Commands::Skill { action } => cli::skill::run(action)?,
        Commands::Completion { shell } => cli::completion::run(shell)?,
        Commands::Man => cli::man::run()?,
    }

    Ok(())
}

fn init_tracing() {
    // Off by default. Users opt in via RUST_LOG / H5I_LOG (e.g.
    // `H5I_LOG=h5i_core=debug`). Writes to stderr so it doesn't poison stdout
    // for piped consumers.
    let filter = tracing_subscriber::EnvFilter::try_from_env("H5I_LOG")
        .or_else(|_| tracing_subscriber::EnvFilter::try_from_default_env())
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("off"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .without_time()
        .try_init();
}

/// `h5i --version --json` prints machine-readable build facts. Handled before
/// clap because clap owns `--version` and would print its own string.
fn maybe_version_json(argv: &[String]) {
    let mut wants_version = false;
    let mut wants_json = false;
    for tok in argv.iter().skip(1) {
        if tok == "--" || !tok.starts_with('-') {
            break;
        }
        match tok.as_str() {
            "--version" | "-V" => wants_version = true,
            "--json" => wants_json = true,
            _ => {}
        }
    }
    if !(wants_version && wants_json) {
        return;
    }
    let out = serde_json::json!({
        "name": "h5i",
        "version": env!("CARGO_PKG_VERSION"),
        "features": Vec::<&str>::new(),
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&out).expect("version json is serializable")
    );
    std::process::exit(0);
}

fn render_man_page<W: std::io::Write>(w: &mut W) -> std::io::Result<()> {
    use std::io::Write as _;
    let cmd = Cli::command();
    let mut buf: Vec<u8> = Vec::new();
    clap_mangen::Man::new(cmd.clone()).render(&mut buf)?;
    append_subcommand_sections(&cmd, "h5i", &mut buf)?;
    writeln!(buf, ".SH SEE ALSO")?;
    writeln!(
        buf,
        "Full narrative manual: \\fBMANUAL.md\\fR in the source tree, or the \
         rendered \\fB/manual/\\fR page on the project site."
    )?;
    // clap_mangen passes help text through verbatim, so typographic Unicode
    // (…, —, →, curly quotes) reaches the roff raw and warns under `-Tascii`.
    // Transliterate to ASCII / roff escapes so the page is clean everywhere.
    w.write_all(sanitize_roff(&String::from_utf8_lossy(&buf)).as_bytes())
}

/// Transliterate typographic Unicode in generated roff to ASCII or roff escapes
/// so the man page renders cleanly under `-Tascii` (existing `\fB`/`\-`/`\(aq`
/// escapes pass through untouched — only non-ASCII scalars are rewritten).
fn sanitize_roff(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '…' => out.push_str("..."),
            '—' => out.push_str("\\(em"),
            '–' => out.push_str("\\(en"),
            '→' => out.push_str("->"),
            '←' => out.push_str("<-"),
            '↔' => out.push_str("<->"),
            '‘' | '’' => out.push('\''),
            '“' | '”' => out.push('"'),
            '•' | '·' => out.push_str("\\(bu"),
            '×' => out.push('x'),
            '≥' => out.push_str(">="),
            '≤' => out.push_str("<="),
            '✔' | '✓' => out.push('+'),
            '✗' | '✘' => out.push('x'),
            c if c.is_ascii() => out.push(c),
            // Anything else exotic (box-drawing, emoji, shading) is dropped to
            // keep the page ASCII-clean; such characters are rare in help text.
            _ => {}
        }
    }
    out
}

/// Append one `.SH` section per visible subcommand (recursively), titled with
/// the full command path. Hidden subcommands are skipped, matching `--help`.
fn append_subcommand_sections<W: std::io::Write>(
    parent: &clap::Command,
    path: &str,
    w: &mut W,
) -> std::io::Result<()> {
    for sub in parent.get_subcommands() {
        if sub.is_hide_set() {
            continue;
        }
        let full = format!("{path} {}", sub.get_name());
        writeln!(w, ".SH \"{}\"", full.to_uppercase())?;
        if let Some(about) = sub.get_about() {
            writeln!(w, "{about}")?;
        }
        // Render this subcommand's synopsis + options into a scratch buffer and
        // demote its top-level `.SH` headings to `.SS` so they nest under the
        // full-path `.SH` above instead of colliding as siblings.
        let man = clap_mangen::Man::new(sub.clone());
        let mut section = Vec::new();
        man.render_synopsis_section(&mut section)?;
        man.render_options_section(&mut section)?;
        w.write_all(&demote_headings(&section))?;
        append_subcommand_sections(sub, &full, w)?;
    }
    Ok(())
}

/// Demote roff section headings (`.SH`) to subsections (`.SS`) at line starts,
/// so a rendered subcommand block nests under its full-path `.SH` heading.
fn demote_headings(bytes: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        match line.strip_prefix(".SH ") {
            Some(rest) => {
                out.push_str(".SS ");
                out.push_str(rest);
            }
            None => out.push_str(line),
        }
    }
    out.into_bytes()
}
