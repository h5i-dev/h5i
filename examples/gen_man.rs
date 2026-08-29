//! Render `docs/man/man1/h5i.1` from the clap command tree, to stdout.
//!
//! An example rather than a subcommand: the page is build output, not a thing
//! a user runs. `h5i man` used to print it, which put a roff generator and
//! `clap_mangen` in everybody's binary to serve a file the project can simply
//! publish — `curl -fsSL https://h5i.dev/man/man1/h5i.1`. Rendering here keeps
//! "never drifts from the actual commands" property (it reads the real
//! `Cli::command()`) and keeps `clap_mangen` in dev-dependencies.
//!
//!     ./scripts/gen_man.sh        # builds this and writes the page

use clap::CommandFactory;

fn main() -> std::io::Result<()> {
    let mut out = std::io::stdout().lock();
    render_man_page(&mut out)
}

fn render_man_page<W: std::io::Write>(w: &mut W) -> std::io::Result<()> {
    use std::io::Write as _;
    let cmd = h5i::Cli::command();
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