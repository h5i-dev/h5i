// Cargo build script. Keeps the box console's React bundle (`web/dist/`)
// present and fresh before `rust-embed` reads it.
//
// It does nothing at all unless the `web` feature is on: a
// `--no-default-features` build has no `server` module, no embedder, and so no
// reason to require Node. When `web` is on, the bundle is rebuilt only when:
//   - web/dist is missing, or
//   - anything under web/src (or index.html / package.json / vite.config.ts /
//     tsconfig.json) is newer than web/dist/index.html
//
// Set `H5I_SKIP_WEB_BUILD=1` to opt out with the feature still on (a developer
// machine running `npm run dev`, or a CI job that builds the frontend itself).
//
// `cargo:rerun-if-changed` lines tell cargo to re-run *this script*. They do
// not by themselves run npm. The freshness check in `main` decides that.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

fn main() {
    // Feature flags reach a build script as CARGO_FEATURE_<NAME>.
    if std::env::var("CARGO_FEATURE_WEB").is_err() {
        return;
    }

    // This crate lives at `crates/h5i-core`; the frontend project is at the
    // workspace root's `web/`, two levels up. `rust-embed` in this crate reads
    // `../../web/dist/`, and this script, which runs before the crate compiles,
    // is what makes that directory exist.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let web = manifest.join("../../web");

    println!("cargo:rerun-if-env-changed=H5I_SKIP_WEB_BUILD");

    if std::env::var("H5I_SKIP_WEB_BUILD").is_ok() {
        eprintln!("h5i build.rs: H5I_SKIP_WEB_BUILD set — skipping frontend build");
        ensure_stub_dist(&web);
        return;
    }

    if !web.exists() {
        // No frontend in this checkout (a slim source export). Still
        // materialize the stub, or rust-embed's derive fails to compile.
        ensure_stub_dist(&web);
        return;
    }

    for rel in [
        "../../web/index.html",
        "../../web/package.json",
        "../../web/package-lock.json",
        "../../web/tsconfig.json",
        "../../web/vite.config.ts",
    ] {
        println!("cargo:rerun-if-changed={}", rel);
    }
    walk_rerun(&web.join("src"));

    let dist_marker = web.join("dist").join("index.html");
    if dist_marker.exists() && !sources_newer_than(&web, &dist_marker) {
        return;
    }

    if !web.join("node_modules").exists() {
        // `ci`, not `install`, whenever a lockfile is committed: `npm install`
        // rewrites package-lock.json, so an ordinary `cargo build` could edit a
        // tracked file as a side effect, and on a machine whose npm records
        // only its own platform's optional binaries, the rewrite it leaves
        // behind is one that fails on every other platform's CI runner. `ci`
        // installs exactly the lockfile and never writes it.
        if web.join("package-lock.json").exists() {
            run_npm(&web, &["ci", "--no-audit", "--no-fund"]);
        } else {
            run_npm(&web, &["install", "--no-audit", "--no-fund"]);
        }
    }
    run_npm(&web, &["run", "build"]);
}

/// `#[derive(RustEmbed)]` fails to compile when its folder is missing, so every
/// path that skips the npm build must still leave a `web/dist/` behind. The
/// stub is written only when no bundle exists. A real one is never touched.
fn ensure_stub_dist(web: &Path) {
    let dist = web.join("dist");
    if let Err(e) = std::fs::create_dir_all(&dist) {
        panic!("failed to create stub {}: {e}", dist.display());
    }
    let marker = dist.join("index.html");
    if !marker.exists() {
        let stub = "<!doctype html><title>h5i</title>\
                    <p>console bundle not built (H5I_SKIP_WEB_BUILD or slim checkout) — \
                    run <code>npm run build</code> in <code>web/</code>.</p>";
        if let Err(e) = std::fs::write(&marker, stub) {
            panic!("failed to write stub {}: {e}", marker.display());
        }
    }
}

fn run_npm(cwd: &Path, args: &[&str]) {
    eprintln!("h5i build.rs: cd {} && npm {}", cwd.display(), args.join(" "));
    match Command::new("npm").args(args).current_dir(cwd).status() {
        Ok(s) if s.success() => {}
        Ok(s) => panic!("npm {:?} failed with exit code {:?}", args, s.code()),
        Err(e) => panic!(
            "failed to invoke npm: {e} (install Node, or build with \
             --no-default-features / H5I_SKIP_WEB_BUILD=1)"
        ),
    }
}

fn walk_rerun(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            walk_rerun(&p);
        } else if let Some(s) = p.to_str() {
            // Non-utf8 paths are skipped: cargo's directive is line-based and
            // could not carry them anyway.
            println!("cargo:rerun-if-changed={}", s);
        }
    }
}

fn sources_newer_than(web: &Path, dist_marker: &Path) -> bool {
    let dist_time = mtime(dist_marker).unwrap_or(SystemTime::UNIX_EPOCH);
    for entry in [
        web.join("index.html"),
        web.join("package.json"),
        web.join("vite.config.ts"),
        web.join("tsconfig.json"),
    ] {
        if let Some(t) = mtime(&entry)
            && t > dist_time
        {
            return true;
        }
    }
    walk_newer_than(&web.join("src"), dist_time)
}

fn walk_newer_than(dir: &Path, threshold: SystemTime) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            if walk_newer_than(&p, threshold) {
                return true;
            }
        } else if let Some(t) = mtime(&p)
            && t > threshold
        {
            return true;
        }
    }
    false
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).ok()?.modified().ok()
}
