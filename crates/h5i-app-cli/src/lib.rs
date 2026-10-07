//! `h5i app`: the development loop of an h5i-app application.
//!
//! An application is a kernel crate (the Rust the proofs are about), a Lake
//! project of proofs, and an `h5i-app.toml` that ties them together. The verbs:
//!
//! | verb | does |
//! |---|---|
//! | `new` | lay out a project that already extracts and proves |
//! | `extract` | kernel -> Charon -> Aeneas -> `proofs/generated/`, plus `schema!`'s Lean |
//! | `check` | `lake build`, then the gate: no `sorry`/`native_decide`/`axiom`, standard axioms only |
//! | `prove` | `extract`, then `check` |
//! | `lint` | routes take an `Actor`, no client-set identity in `Command` |
//! | `mutate` | inject bugs, re-extract, rebuild: every bug must break a proof |
//! | `doctor` | tools and pins |
//!
//! The `h5i` binary mounts this as `h5i app`; in the h5i repository
//! `cargo app` runs the same commands without building the browser.

use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::Subcommand;

pub mod check;
pub mod doctor;
pub mod extract;
pub mod lint;
pub mod manifest;
pub mod mutate;
pub mod new;
pub mod packages;
pub mod pins;
pub mod util;

use manifest::Project;
use util::Out;

#[derive(Subcommand, Debug)]
pub enum AppCommands {
    /// Start a project: a kernel crate, a Lake project that proves it, and
    /// the h5i-app.toml that ties them together.
    New {
        /// Directory to create.
        dir: PathBuf,
        /// Crate name prefix (default: the directory's name).
        #[arg(long)]
        name: Option<String>,
        /// Require the h5i-app Lean library from this local h5i checkout
        /// instead of from git.
        #[arg(long, value_name = "H5I_CHECKOUT", conflicts_with = "rev")]
        h5i_path: Option<PathBuf>,
        /// The h5i git revision to require the Lean library at (default: this
        /// binary's release tag).
        #[arg(long)]
        rev: Option<String>,
    },

    /// Extract the kernel to Lean: Rust -> LLBC (Charon) -> Lean (Aeneas),
    /// into proofs/generated/. With `schema = true`, also write the Lean
    /// that `schema!` renders to its `lean "..."` path.
    Extract {
        #[command(flatten)]
        targets: Targets,
        /// Change nothing; fail if proofs/generated/ is not what the kernel
        /// extracts to (for CI).
        #[arg(long)]
        check: bool,
    },

    /// Build the proofs and gate them: no sorry, native_decide or axiom, and
    /// every theorem in a hand-written module uses only propext,
    /// Classical.choice and Quot.sound.
    Check {
        #[command(flatten)]
        targets: Targets,
        #[command(flatten)]
        fetch: FetchArgs,
    },

    /// `extract`, then `check`.
    Prove {
        #[command(flatten)]
        targets: Targets,
        #[command(flatten)]
        fetch: FetchArgs,
    },

    /// Check the source for what the proofs assume but cannot see: every
    /// mutating route takes an authenticated `Actor`, and no `Command` field
    /// lets the client set its own identity or privilege. Reports whether the
    /// proofs state a universal authorization theorem.
    Lint(Targets),

    /// Put bugs in the kernel and make sure each one breaks a proof. A bug
    /// that survives is behaviour the spec does not pin down.
    Mutate {
        #[command(flatten)]
        targets: Targets,
        #[command(flatten)]
        args: MutateArgs,
    },

    /// Check that Charon, Aeneas and Lean are installed at the versions this
    /// h5i extracts with, and that the project pins the same ones.
    Doctor {
        /// Project directory or h5i-app.toml (default: the one the current
        /// directory is in, if any).
        path: Option<PathBuf>,
    },
}

/// Which projects a verb runs on.
#[derive(clap::Args, Debug)]
pub struct Targets {
    /// Project directories or h5i-app.toml files (default: the project the
    /// current directory is in).
    paths: Vec<PathBuf>,
    /// Every project under the given directories (default: the current one).
    #[arg(long)]
    all: bool,
}

impl Targets {
    pub fn projects(&self) -> Result<Vec<Project>> {
        if self.all {
            let dirs = if self.paths.is_empty() {
                vec![std::env::current_dir()?]
            } else {
                self.paths.clone()
            };
            let mut out = Vec::new();
            for d in dirs {
                out.extend(Project::discover(&d)?);
            }
            if out.is_empty() {
                bail!("no {} found", manifest::FILE);
            }
            return Ok(out);
        }
        if self.paths.is_empty() {
            return Ok(vec![Project::find(None)?]);
        }
        self.paths.iter().map(|p| Project::find(Some(p))).collect()
    }
}

/// Where `check` gets the Lake packages: Aeneas, Mathlib and the rest,
/// shared by every project with the same requires in
/// `$XDG_CACHE_HOME/h5i/lake` (`H5I_LAKE_CACHE` names another directory, or
/// `off` for a copy in each project).
#[derive(clap::Args, Debug, Default)]
pub struct FetchArgs {
    /// Discard the project's Lake packages and fetch them again, for a fetch
    /// that left them broken. Shared packages are discarded for every
    /// project that uses them.
    #[arg(long)]
    pub refetch: bool,
}

/// How `mutate` runs.
#[derive(clap::Args, Debug, Default)]
pub struct MutateArgs {
    /// Only mutants whose name contains this (repeatable).
    #[arg(long, value_name = "TEXT")]
    pub only: Vec<String>,
    /// Also generate mutants from the kernel's syntax: comparison
    /// boundaries, flipped equalities, swapped `&&`/`||`, dropped `!`,
    /// forced `if` conditions.
    #[arg(long)]
    pub auto: bool,
    /// With --auto: at most this many generated mutants per project.
    #[arg(long)]
    pub limit: Option<usize>,
    /// List the mutants and exit.
    #[arg(long)]
    pub list: bool,
    /// Mutants to run at once. Each is a full Lean build; mind memory.
    #[arg(short, long, default_value_t = 1)]
    pub jobs: usize,
    /// Keep each mutant's directory and log.
    #[arg(long)]
    pub keep: bool,
    /// Skip building the unmodified copy first.
    #[arg(long)]
    pub no_baseline: bool,
}

/// What the caller knows that this crate does not.
pub struct Context {
    /// The h5i revision whose Lean library `new` requires by default: this
    /// binary's release tag. `new` falls back to `main` when the h5i
    /// repository has no such tag (a build between releases).
    pub default_rev: String,
}

pub fn run(cmd: AppCommands, cx: &Context) -> Result<()> {
    let out = Out::Inherit;
    // `--all --refetch`: projects that share packages fetch them once.
    let refetched = std::cell::RefCell::new(Vec::new());
    let refetch = |p: &Project, fetch: &FetchArgs| -> Result<bool> {
        if !fetch.refetch {
            return Ok(false);
        }
        let dir = packages::Packages::locate(&p.proofs(), packages::cache_root().as_deref())?.dir;
        let mut seen = refetched.borrow_mut();
        Ok(if seen.contains(&dir) { false } else { seen.push(dir); true })
    };
    match cmd {
        AppCommands::New {
            dir,
            name,
            h5i_path,
            rev,
        } => {
            let lib = match h5i_path {
                Some(p) => new::Lib::Path(p),
                None => new::Lib::Git(match rev {
                    Some(r) => r,
                    None => new::default_rev(&cx.default_rev),
                }),
            };
            new::new(&dir, name.as_deref(), &lib)
        }
        AppCommands::Extract { targets: t, check } => each(&t, |p| {
            if t.all && p.manifest.extract.is_none() {
                return Ok(());
            }
            if check { extract::check_fresh(p, &out) } else { extract::extract(p, &out) }
        }),
        AppCommands::Lint(t) => each(&t, |p| {
            lint::lint(p, &out)?;
            eprintln!("{}: lint ok", p.display());
            Ok(())
        }),
        AppCommands::Check { targets: t, fetch } => each(&t, |p| {
            check::check(p, refetch(p, &fetch)?, &out)?;
            eprintln!("{}: proofs ok", p.display());
            Ok(())
        }),
        AppCommands::Prove { targets: t, fetch } => each(&t, |p| {
            if p.manifest.extract.is_some() {
                extract::extract(p, &out)?;
            }
            check::check(p, refetch(p, &fetch)?, &out)?;
            eprintln!("{}: proofs ok", p.display());
            Ok(())
        }),
        AppCommands::Mutate { targets: t, args } => each(&t, |p| {
            if t.all && p.manifest.mutants.is_empty() && !(args.auto && p.manifest.extract.is_some()) {
                return Ok(());
            }
            mutate_project(p, &args)
        }),
        AppCommands::Doctor { path } => {
            let p = match path {
                Some(p) => Some(Project::find(Some(&p))?),
                // No project is fine (doctor checks the tools); a manifest
                // that is there but does not parse is not.
                None => match manifest::locate(&std::env::current_dir()?) {
                    Some(f) => Some(Project::load(&f)?),
                    None => None,
                },
            };
            doctor::doctor(p.as_ref())
        }
    }
}

/// `mutate` on one project.
pub fn mutate_project(p: &Project, args: &MutateArgs) -> Result<()> {
    let wanted = |m: &mutate::Mutant| args.only.is_empty() || args.only.iter().any(|o| m.name.contains(o.as_str()));
    let mut mutants = mutate::declared(p)?;
    mutants.retain(&wanted);
    if args.auto {
        // Filter first, so `--limit` counts the mutants that were asked for.
        let mut g = mutate::generated(p)?;
        g.retain(&wanted);
        if let Some(n) = args.limit {
            g.truncate(n);
        }
        mutants.extend(g);
    }
    if args.list {
        for m in &mutants {
            match &m.what {
                Some(w) => println!("{:<40} {w}", m.name),
                None => println!("{}", m.name),
            }
        }
        return Ok(());
    }
    if mutants.is_empty() {
        bail!("{}: no mutants. Add [[mutant]] entries to h5i-app.toml, or pass --auto", p.display());
    }
    let opts = mutate::Options { jobs: args.jobs, keep: args.keep, baseline: !args.no_baseline };
    summarize(&mutate::run(p, mutants, &opts)?)
}

/// Run `f` on every target project; keep going past a failure and report
/// them all at the end.
fn each(t: &Targets, f: impl Fn(&Project) -> Result<()>) -> Result<()> {
    let projects = t.projects()?;
    let many = projects.len() > 1;
    let mut failed = Vec::new();
    for p in &projects {
        if many {
            eprintln!("==> {}", p.display());
        }
        if let Err(e) = f(p) {
            if !many {
                return Err(e);
            }
            eprintln!("error: {e:#}");
            failed.push(p.display());
        }
    }
    if !failed.is_empty() {
        bail!(
            "{} of {} projects failed: {}",
            failed.len(),
            projects.len(),
            failed.join(", ")
        );
    }
    Ok(())
}

/// Print the tally; fail if a mutant survived or a declared one was invalid.
pub fn summarize(results: &[mutate::Outcome]) -> Result<()> {
    use mutate::Verdict;
    let caught = results
        .iter()
        .filter(|o| o.verdict == Verdict::Caught)
        .count();
    let survived: Vec<&mutate::Outcome> = results
        .iter()
        .filter(|o| o.verdict == Verdict::Survived)
        .collect();
    let invalid = results
        .iter()
        .filter(|o| matches!(o.verdict, Verdict::Invalid(_)))
        .count();
    eprintln!(
        "\n{caught}/{} caught, {} survived, {invalid} invalid",
        results.len(),
        survived.len()
    );
    for o in &survived {
        eprintln!("  survived: {}", o.name);
    }
    // A generated mutant that does not compile or extract says nothing about
    // the spec; a declared one that does not is a broken manifest.
    let broken: Vec<&mutate::Outcome> = results
        .iter()
        .filter(|o| matches!(o.verdict, Verdict::Invalid(_)) && !o.generated)
        .collect();
    for o in &broken {
        eprintln!(
            "  invalid: {} ({}; fix its entry in h5i-app.toml)",
            o.name, o.verdict
        );
    }
    if !survived.is_empty() || !broken.is_empty() {
        bail!(
            "{} mutant(s) survived, {} declared mutant(s) invalid",
            survived.len(),
            broken.len()
        );
    }
    Ok(())
}
