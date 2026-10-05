//! `cargo app-verify`: CI's h5i-app checks, run locally over every
//! `h5i-app.toml` in the repository (`--full` adds the mutants and the
//! differential test). Missing tools count as skipped, never passed.
//!
//! It is a composition, not a second implementation: each project goes through
//! the same `h5i app lint`, `extract --check`, `check` and `mutate` an
//! application developer runs, and only what belongs to this repository is
//! added here (the PostgreSQL test run, the `cargo deny` bans of A8, the docs
//! differential test).
//!
//! `cargo app <verb>` is `h5i app <verb>` for contributors: the same code
//! without building the h5i binary and its browser engine.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::Instant;

use h5i_app_cli::manifest::Project;
use h5i_app_cli::util::Out;

#[derive(PartialEq)]
enum Outcome {
    Pass,
    Fail(String),
    Skip(String),
}

struct Ctx {
    root: PathBuf,
    results: Vec<(String, Outcome, f32)>,
}

impl Ctx {
    fn step(&mut self, name: &str, f: impl FnOnce(&Path) -> Outcome) {
        eprint!("{name:<44} ");
        let t = Instant::now();
        let out = f(&self.root);
        let secs = t.elapsed().as_secs_f32();
        match &out {
            Outcome::Pass => eprintln!("ok      {secs:>6.1}s"),
            Outcome::Skip(why) => eprintln!("skipped ({why})"),
            Outcome::Fail(log) => eprintln!("FAILED  {secs:>6.1}s\n{log}"),
        }
        self.results.push((name.to_string(), out, secs));
    }
}

/// Run a command; on failure return the last lines of its output.
fn run(dir: &Path, prog: &str, args: &[&str]) -> Outcome {
    match Command::new(prog).args(args).current_dir(dir).stdin(Stdio::null()).output() {
        Err(e) => Outcome::Fail(format!("cannot run {prog}: {e}")),
        Ok(o) if o.status.success() => Outcome::Pass,
        Ok(o) => {
            let text = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
            let tail: Vec<&str> = text.lines().rev().take(30).collect();
            Outcome::Fail(tail.into_iter().rev().collect::<Vec<_>>().join("\n"))
        }
    }
}

/// True if `prog` is an executable on PATH (charon and aeneas have no `--version`).
fn have(prog: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| std::env::split_paths(&paths).any(|d| d.join(prog).is_file()))
}

fn have_cargo_sub(sub: &str) -> bool {
    Command::new("cargo").args([sub, "--version"]).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}


/// Run `f` with its output in a log; on failure, the log's tail and the error.
fn logged(f: impl FnOnce(&Out) -> anyhow::Result<()>) -> Outcome {
    let log = match tempfile::NamedTempFile::new() {
        Ok(l) => l,
        Err(e) => return Outcome::Fail(e.to_string()),
    };
    let out = Out::Log(log.path().to_path_buf());
    match f(&out) {
        Ok(()) => Outcome::Pass,
        Err(e) => {
            let text = std::fs::read_to_string(log.path()).unwrap_or_default();
            let tail: Vec<&str> = text.lines().rev().take(30).collect();
            Outcome::Fail(format!(
                "{}\n{e:#}",
                tail.into_iter().rev().collect::<Vec<_>>().join("\n")
            ))
        }
    }
}


#[derive(clap::Parser)]
#[command(name = "cargo app", bin_name = "cargo app", about = "h5i app, for contributors to this repository")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(clap::Subcommand)]
enum Cmd {
    /// `cargo app-verify`: CI's checks over every project in the repository.
    #[command(hide = true)]
    Verify {
        /// Add the mutants and the differential test.
        #[arg(long)]
        full: bool,
        /// Do not re-extract (no Charon or Aeneas needed).
        #[arg(long)]
        no_extract: bool,
    },
    #[command(flatten)]
    App(h5i_app_cli::AppCommands),
}

fn main() -> ExitCode {
    use clap::Parser;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().expect("repository root");
    match Cli::parse().cmd {
        Cmd::Verify { full, no_extract } => verify(root, full, !no_extract),
        Cmd::App(cmd) => match h5i_app_cli::run(cmd, &h5i_app_cli::Context { default_rev: "main".into() }) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e:#}");
                ExitCode::FAILURE
            }
        },
    }
}

fn verify(root: PathBuf, full: bool, extract: bool) -> ExitCode {
    let projects = match Project::discover(&root.join("crates")).and_then(|mut c| {
        c.extend(Project::discover(&root.join("examples/app"))?);
        Ok(c)
    }) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e:#}");
            return ExitCode::FAILURE;
        }
    };
    let base = root.clone();
    let rel = |p: &Project| p.root.strip_prefix(&base).unwrap_or(&p.root).display().to_string();
    let mut cx = Ctx { root, results: Vec::new() };
    let db = std::env::var("H5I_APP_TEST_DATABASE_URL").is_ok();

    for p in &projects {
        cx.step(&format!("lint {}", rel(p)), |_| logged(|out| h5i_app_cli::lint::lint(p, out)));
    }
    // Never fails; makes the gaps visible.
    let authz: Vec<(String, String)> =
        projects.iter().filter_map(|p| h5i_app_cli::lint::authz_coverage(p).map(|n| (rel(p), n))).collect();
    let proven = authz.iter().filter(|(_, n)| n.contains("proven")).count();
    eprintln!("\n  authorization coverage: {proven}/{} kernels with commands", authz.len());
    for (p, n) in &authz {
        eprintln!("  {p:<34} {}", n.trim_start_matches("authorization: "));
    }
    eprintln!();
    cx.step("rust tests", |r| {
        if !db {
            return Outcome::Skip("H5I_APP_TEST_DATABASE_URL unset".into());
        }
        run(r, "bash", &["scripts/app/ci-rust-tests.sh", "--release"])
    });
    // A8: only h5i-app-pg may depend on database drivers.
    cx.step("cargo deny (bans)", |r| {
        if !have_cargo_sub("deny") {
            return Outcome::Skip("cargo-deny not installed".into());
        }
        match run(r, "cargo", &["deny", "check", "bans"]) {
            Outcome::Pass => run(&r.join("examples/app"), "cargo", &["deny", "check", "bans"]),
            other => other,
        }
    });
    if extract {
        let tools = have("charon") && have("aeneas");
        for p in projects.iter().filter(|p| p.manifest.extract.is_some()) {
            // A9: the committed Lean is exactly what Aeneas extracts.
            cx.step(&format!("extract --check {}", rel(p)), |_| {
                if !tools {
                    return Outcome::Skip("charon/aeneas not on PATH".into());
                }
                logged(|out| h5i_app_cli::extract::check_fresh(p, out))
            });
        }
    }
    let lake = have("lake");
    for p in &projects {
        cx.step(&format!("check {}", rel(p)), |_| {
            if !lake {
                return Outcome::Skip("lake not on PATH".into());
            }
            logged(|out| h5i_app_cli::check::check(p, out))
        });
    }
    if full {
        let args = h5i_app_cli::MutateArgs { jobs: 2, ..Default::default() };
        for p in projects.iter().filter(|p| !p.manifest.mutants.is_empty()) {
            cx.step(&format!("mutants {}", rel(p)), |_| {
                eprintln!();
                match h5i_app_cli::mutate_project(p, &args) {
                    Ok(()) => Outcome::Pass,
                    Err(e) => Outcome::Fail(format!("{e:#}")),
                }
            });
        }
        cx.step("rust vs lean differential test", |r| run(r, "bash", &["scripts/app/difftest.sh"]));
    }

    let failed = cx.results.iter().filter(|(_, o, _)| matches!(o, Outcome::Fail(_))).count();
    let skipped = cx.results.iter().filter(|(_, o, _)| matches!(o, Outcome::Skip(_))).count();
    eprintln!("\n{} passed, {failed} failed, {skipped} skipped", cx.results.len() - failed - skipped);
    if failed > 0 { ExitCode::FAILURE } else { ExitCode::SUCCESS }
}
