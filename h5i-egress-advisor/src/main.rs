//! h5i-egress-advisor — turn refused egress into allowlist candidates.
//!
//! ```text
//! h5i box export mybox --out ./review
//! h5i-egress-advisor ./review/receipt.json
//! h5i-egress-advisor --box mybox
//! ```
//!
//! It reads receipts and prints. It never runs `h5i`, never edits a policy,
//! and never touches the store it read from.

mod advise;
mod receipt;
mod render;

use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;

const VERSION: &str = env!("CARGO_PKG_VERSION");

const USAGE: &str = "\
h5i-egress-advisor — what the egress allowlist refused, and what to do about it

USAGE:
    h5i-egress-advisor <RECEIPT>       an export bundle, a receipt.jsonl, or the directory of either
    h5i-egress-advisor --box <NAME>    the live receipt of a box in this repository's store

OPTIONS:
    --box <NAME>       read a box's log; <NAME> is a slug or <agent>/<slug>
    --root <DIR>       the h5i store to look in (default: this repository's .git/.h5i)
    --json             machine-readable report on stdout
    --toml             a [profile.X.net] block for .h5i/env.toml, for boxes `h5i box allow`
                       cannot reach (supervised and process tiers)
    --profile <NAME>   the profile name to write in --toml output
    --min <N>          only report destinations refused at least N times (default: 1)
    --no-color         never colorize (also honoured: NO_COLOR)
    -h, --help         print this
    -V, --version      print the version

EXIT STATUS:
    0   read the receipt, nothing was refused
    1   read the receipt, refusals reported above
    2   could not read a receipt

Nothing it prints is executed. Widening a boundary is a decision, and the
decision is yours.
";

/// What the command line asked for.
struct Args {
    path: Option<PathBuf>,
    boxname: Option<String>,
    root: Option<PathBuf>,
    json: bool,
    toml: bool,
    profile: Option<String>,
    min: u64,
    no_color: bool,
}

enum Parsed {
    Run(Box<Args>),
    Help,
    Version,
    Error(String),
}

fn parse_args(argv: Vec<String>) -> Parsed {
    let mut a = Args {
        path: None,
        boxname: None,
        root: None,
        json: false,
        toml: false,
        profile: None,
        min: 1,
        no_color: false,
    };
    let mut it = argv.into_iter();
    // A value-taking flag with nothing after it is a mistake worth naming,
    // rather than a silent default.
    macro_rules! value {
        ($flag:expr, $it:expr) => {
            match $it.next() {
                Some(v) if !v.starts_with('-') => v,
                _ => return Parsed::Error(format!("{} needs a value", $flag)),
            }
        };
    }
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => return Parsed::Help,
            "-V" | "--version" => return Parsed::Version,
            "--json" => a.json = true,
            "--toml" => a.toml = true,
            "--no-color" => a.no_color = true,
            "--box" => a.boxname = Some(value!("--box", it)),
            "--root" => a.root = Some(PathBuf::from(value!("--root", it))),
            "--profile" => a.profile = Some(value!("--profile", it)),
            "--min" => {
                let v = value!("--min", it);
                match v.parse::<u64>() {
                    Ok(n) => a.min = n.max(1),
                    Err(_) => return Parsed::Error(format!("--min wants a number, got '{v}'")),
                }
            }
            "--" => {}
            other if other.starts_with('-') && other != "-" => {
                return Parsed::Error(format!("unknown option '{other}'"));
            }
            other => {
                if a.path.is_some() {
                    return Parsed::Error(format!("unexpected second receipt '{other}'"));
                }
                a.path = Some(PathBuf::from(other));
            }
        }
    }
    if a.json && a.toml {
        return Parsed::Error("--json and --toml are two different reports; pick one".into());
    }
    match (&a.path, &a.boxname) {
        (Some(_), Some(_)) => Parsed::Error("pass a receipt path or --box, not both".into()),
        (None, None) => {
            Parsed::Error("nothing to read — pass a receipt path or --box <NAME>".into())
        }
        _ => Parsed::Run(Box::new(a)),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let a = match parse_args(args) {
        Parsed::Run(a) => a,
        Parsed::Help => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Parsed::Version => {
            println!("h5i-egress-advisor {VERSION}");
            return ExitCode::SUCCESS;
        }
        Parsed::Error(msg) => {
            eprintln!("h5i-egress-advisor: {msg}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let receipts = match (&a.path, &a.boxname) {
        (Some(p), _) => receipt::load_path(p),
        (_, Some(name)) => receipt::load_box(name, a.root.as_deref(), &cwd),
        _ => unreachable!("parse_args refuses this"),
    };
    let receipts = match receipts {
        Ok(r) => r,
        Err(e) => {
            eprintln!("h5i-egress-advisor: {e}");
            return ExitCode::from(2);
        }
    };

    let advice = advise::advise(&receipts, a.min);
    let stdout = std::io::stdout();
    let color = !a.no_color && std::env::var_os("NO_COLOR").is_none() && stdout.is_terminal();
    let out = if a.json {
        render::json(&advice, VERSION)
    } else if a.toml {
        render::toml(&advice, a.profile.as_deref())
    } else {
        render::text(&advice, &render::Style { color })
    };
    // A closed pipe (`| head`) is not an error worth a message.
    if write!(stdout.lock(), "{out}").is_err() {
        return ExitCode::from(0);
    }

    if advice.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &[&str]) -> Parsed {
        parse_args(s.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn a_bare_path_is_the_receipt() {
        match args(&["./review/receipt.json"]) {
            Parsed::Run(a) => {
                assert_eq!(a.path.unwrap(), PathBuf::from("./review/receipt.json"));
                assert_eq!(a.min, 1);
                assert!(!a.json && !a.toml);
            }
            _ => panic!("expected a run"),
        }
    }

    #[test]
    fn flags_are_read() {
        match args(&[
            "--box",
            "mybox",
            "--toml",
            "--profile",
            "review",
            "--min",
            "3",
        ]) {
            Parsed::Run(a) => {
                assert_eq!(a.boxname.as_deref(), Some("mybox"));
                assert!(a.toml);
                assert_eq!(a.profile.as_deref(), Some("review"));
                assert_eq!(a.min, 3);
            }
            _ => panic!("expected a run"),
        }
    }

    #[test]
    fn the_mistakes_worth_naming_are_named() {
        for bad in [
            vec![],
            vec!["--box"],
            vec!["--min", "lots"],
            vec!["--nope"],
            vec!["a.json", "b.json"],
            vec!["a.json", "--box", "mybox"],
            vec!["a.json", "--json", "--toml"],
        ] {
            assert!(
                matches!(args(&bad), Parsed::Error(_)),
                "{bad:?} should not have parsed"
            );
        }
    }

    #[test]
    fn help_and_version_win_over_everything_else() {
        assert!(matches!(args(&["--box", "x", "--help"]), Parsed::Help));
        assert!(matches!(args(&["-V"]), Parsed::Version));
    }
}
