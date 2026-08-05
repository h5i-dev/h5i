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
// `Env`'s clap enum is much larger than the two generator commands; boxing it
// would break clap's derive, and the enum is constructed once per process.
#[allow(clippy::large_enum_variant)]
enum Commands {
    /// Isolated agent environments: a confined worktree with a pinned,
    /// fail-closed policy. Run `h5i env --help` for the verb table.
    Env {
        #[command(subcommand)]
        action: cli::env::EnvCommands,
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

fn main() -> anyhow::Result<()> {
    init_tracing();
    let argv: Vec<String> = std::env::args().collect();
    maybe_version_json(&argv);
    let cli = Cli::parse_from(argv);

    match cli.command {
        Commands::Env { action } => cli::env::run(action)?,
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
