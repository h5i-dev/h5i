//! Compile the eBPF probe, when this host can.
//!
//! The build is deliberately *soft*. A missing or BPF-incapable `clang` does
//! not fail the build; it leaves the object out, and the loader then reports
//! `unavailable: built without the eBPF object` at run time rather than
//! pretending it collected nothing. That keeps `cargo build` working for a
//! contributor who has no LLVM installed and is only touching the CLI, which
//! is the ordinary case.
//!
//! It is soft in exactly one direction, though. `H5I_BPF_REQUIRE=1` turns
//! every skip below into a hard failure, and that is what the CI job for this
//! lane sets. A binary that silently shipped without its detector would be the
//! worst of both worlds: a `runtime` block that always says `unavailable` and
//! a reader who takes that for a quiet box.
//!
//! The released binaries do *not* carry the probe today, and that is stated
//! rather than left to be discovered: the release matrix cross-builds musl
//! targets inside containers that have no LLVM, and `h5i box detect probe`
//! reports the consequence in one line with the command that fixes it
//! (`cargo install --path . --features bpf`). Putting a BPF-capable clang into
//! four cross-build images to ship a feature that also needs `CAP_BPF` on the
//! user's machine is work that should follow somebody wanting it, not precede
//! them.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Set to `1` to turn every "skipped, and here is why" below into a build
/// failure.
const REQUIRE_VAR: &str = "H5I_BPF_REQUIRE";

fn main() {
    println!("cargo::rerun-if-changed=bpf/h5i_detect.bpf.c");
    println!("cargo::rerun-if-changed=bpf/h5i_bpf.h");
    println!("cargo::rerun-if-changed=bpf/h5i_event.h");
    println!("cargo::rerun-if-env-changed={REQUIRE_VAR}");
    println!("cargo::rerun-if-env-changed=CLANG");
    // Declared so the `unexpected_cfgs` lint stays quiet under `-D warnings`.
    println!("cargo::rustc-check-cfg=cfg(h5i_bpf_object)");

    // The object is only ever loaded on Linux, and only by the `load` feature.
    // Building it anywhere else would cost every macOS contributor a clang
    // invocation for a file nothing reads.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("linux") {
        return;
    }
    if std::env::var_os("CARGO_FEATURE_LOAD").is_none() {
        return;
    }

    match build_object() {
        Ok(path) => {
            println!("cargo::rustc-env=H5I_BPF_OBJECT={}", path.display());
            println!("cargo::rustc-cfg=h5i_bpf_object");
        }
        Err(why) => skip(&why),
    }
}

/// Report a skip, or fail the build when the caller asked for the object.
fn skip(why: &str) -> ! {
    if std::env::var(REQUIRE_VAR).as_deref() == Ok("1") {
        panic!(
            "{REQUIRE_VAR}=1 but the eBPF probe could not be built: {why}\n\
             Install an LLVM whose clang can target BPF (Debian/Ubuntu: `apt install clang llvm`), \
             or unset {REQUIRE_VAR} to build h5i without the runtime-detection lane."
        );
    }
    println!("cargo::warning=h5i-bpf: runtime detection disabled — {why}");
    // Not an error: the crate compiles, `h5i box detect probe` says why, and
    // every `runtime` block it writes carries the same reason.
    std::process::exit(0);
}

fn build_object() -> Result<PathBuf, String> {
    let clang = find_clang().ok_or_else(|| {
        "no clang on PATH (looked for $CLANG, clang, clang-20 … clang-14)".to_string()
    })?;

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").map_err(|e| e.to_string())?);
    let obj = out_dir.join("h5i_detect.o");
    let src = Path::new("bpf/h5i_detect.bpf.c");

    let output = Command::new(&clang)
        .args([
            "-target",
            "bpf",
            // -O2 is not a preference: the verifier rejects the unoptimized
            // output of essentially every BPF program, because -O0 spills to
            // the stack in patterns it cannot follow.
            "-O2",
            // -g emits .BTF, which is what carries the `.maps` section's map
            // definitions. Without it aya finds no maps at all. It also emits
            // DWARF, which nothing reads and which `strip_dwarf` removes below
            // when the toolchain has the tool for it.
            "-g",
            "-Wall",
            "-Werror",
            "-c",
        ])
        .arg(src)
        .arg("-o")
        .arg(&obj)
        .output()
        .map_err(|e| format!("could not run {}: {e}", clang.display()))?;

    if !output.status.success() {
        return Err(format!(
            "{} failed: {}",
            clang.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    if !obj.is_file() {
        return Err(format!("{} produced no object", clang.display()));
    }
    strip_dwarf(&clang, &obj);
    Ok(obj)
}

/// Drop the DWARF sections, keeping `.BTF`/`.BTF.ext`.
///
/// Best effort by design: DWARF is roughly 90% of the object and nothing in
/// h5i reads it, but an object that still carries it loads identically. So a
/// missing `llvm-strip` is not worth failing a build over.
fn strip_dwarf(clang: &Path, obj: &Path) {
    // Prefer the `llvm-strip` that sits beside the clang we found, so a
    // versioned toolchain is not mixed with the distribution default.
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(dir) = clang.parent() {
        let stem = clang.file_name().and_then(|s| s.to_str()).unwrap_or("clang");
        let suffix = stem.strip_prefix("clang").unwrap_or("");
        candidates.push(dir.join(format!("llvm-strip{suffix}")));
    }
    candidates.push(PathBuf::from("llvm-strip"));

    for cand in candidates {
        if matches!(
            Command::new(&cand).arg("-g").arg(obj).status(),
            Ok(status) if status.success()
        ) {
            return;
        }
    }
}

/// Find a clang that can actually emit BPF.
///
/// Presence is not the question, a clang built without the BPF backend
/// compiles the file and then fails at codegen, so each candidate is asked to
/// compile an empty translation unit for the BPF target before it is trusted.
fn find_clang() -> Option<PathBuf> {
    let mut names: Vec<String> = Vec::new();
    if let Ok(explicit) = std::env::var("CLANG") {
        names.push(explicit);
    }
    names.push("clang".to_string());
    // Newest first: a host with both clang-14 and clang-20 should use the one
    // whose BPF backend knows about the newer instruction set.
    for v in (14..=20).rev() {
        names.push(format!("clang-{v}"));
    }

    for name in names {
        let path = PathBuf::from(&name);
        if targets_bpf(&path) {
            return Some(path);
        }
    }
    None
}

fn targets_bpf(clang: &Path) -> bool {
    let out_dir = std::env::var("OUT_DIR").unwrap_or_else(|_| ".".to_string());
    let probe_c = Path::new(&out_dir).join("h5i_bpf_probe.c");
    let probe_o = Path::new(&out_dir).join("h5i_bpf_probe.o");
    if std::fs::write(&probe_c, "int h5i_probe(void) { return 0; }\n").is_err() {
        return false;
    }
    let ok = Command::new(clang)
        .args(["-target", "bpf", "-O2", "-c"])
        .arg(&probe_c)
        .arg("-o")
        .arg(&probe_o)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    let _ = std::fs::remove_file(&probe_c);
    let _ = std::fs::remove_file(&probe_o);
    ok
}
