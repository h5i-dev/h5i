// Cargo build script.
//
// Ensures the React workbench bundle (web/dist/) exists and is fresh before
// `rust-embed` reads it. We rebuild only when:
//   - web/dist is missing, or
//   - any file under web/src or web/index.html or web/package.json or
//     web/vite.config.ts is newer than web/dist/index.html
//
// Set `H5I_SKIP_WEB_BUILD=1` to opt out (e.g. on a developer machine that
// rebuilds the frontend manually with `npm run dev`).
//
// `cargo:rerun-if-changed` lines tell cargo to re-run *this script* — they do
// not by themselves run npm. The freshness check inside main() decides
// whether to invoke npm.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

fn main() {
    // This crate (h5i-core) lives at `crates/h5i-core`; the frontend project is
    // at the workspace root's `web/`, two levels up. `rust-embed` in this crate
    // reads `../../web/dist/`, and this build script (which runs *before* this
    // crate compiles, unlike a build script on the top-level bin) is what makes
    // that dist exist.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let web = manifest.join("../../web");

    if std::env::var("H5I_SKIP_WEB_BUILD").is_ok() {
        eprintln!("h5i build.rs: H5I_SKIP_WEB_BUILD set — skipping frontend build");
        ensure_stub_dist(&web);
        return;
    }

    if !web.exists() {
        // No frontend in this checkout (e.g. a slim source export) — still
        // materialize the stub, or rust-embed's derive fails to compile.
        ensure_stub_dist(&web);
        return;
    }

    // Tell cargo to re-evaluate this script when relevant frontend inputs change
    // (paths relative to this crate's manifest dir → `../../web/...`).
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
    println!("cargo:rerun-if-env-changed=H5I_SKIP_WEB_BUILD");

    let dist_marker = web.join("dist").join("index.html");
    let needs_build = !dist_marker.exists() || sources_newer_than(&web, &dist_marker);
    if !needs_build {
        return;
    }

    // node_modules absent → run npm ci (or install) once.
    if !web.join("node_modules").exists() {
        run_npm(&web, &["install", "--no-audit", "--no-fund"]);
    }

    run_npm(&web, &["run", "build"]);
}

/// `#[derive(RustEmbed)]` fails to compile when its folder is missing, so
/// every path that skips the npm build (H5I_SKIP_WEB_BUILD, no `web/` in the
/// checkout) must still leave a `web/dist/` behind. A stub index.html is
/// written only when none exists — a real bundle is never touched.
fn ensure_stub_dist(web: &Path) {
    let dist = web.join("dist");
    if let Err(e) = std::fs::create_dir_all(&dist) {
        panic!("failed to create stub {}: {e}", dist.display());
    }
    let marker = dist.join("index.html");
    if !marker.exists() {
        let stub = "<!doctype html><title>h5i</title>\
                    <p>frontend not built (H5I_SKIP_WEB_BUILD or slim checkout) — \
                    run <code>npm run build</code> in <code>web/</code>.</p>";
        if let Err(e) = std::fs::write(&marker, stub) {
            panic!("failed to write stub {}: {e}", marker.display());
        }
    }
}

fn run_npm(cwd: &Path, args: &[&str]) {
    eprintln!("h5i build.rs: cd {} && npm {}", cwd.display(), args.join(" "));
    let status = Command::new("npm").args(args).current_dir(cwd).status();
    match status {
        Ok(s) if s.success() => {}
        Ok(s) => panic!("npm {:?} failed with exit code {:?}", args, s.code()),
        Err(e) => panic!("failed to invoke npm: {} (is Node installed?)", e),
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
        } else {
            // PathBuf -> str: skip non-utf8 paths (extremely unlikely here).
            if let Some(s) = p.to_str() {
                println!("cargo:rerun-if-changed={}", s);
            }
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
        if let Some(t) = mtime(&entry) {
            if t > dist_time {
                return true;
            }
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
        } else if let Some(t) = mtime(&p) {
            if t > threshold {
                return true;
            }
        }
    }
    false
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).ok()?.modified().ok()
}
