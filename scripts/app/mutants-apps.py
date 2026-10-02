#!/usr/bin/env python3
"""Re-extract each non-docs kernel with one plausible bug and rebuild its proofs.

Rust compilation and extraction must succeed before a proof failure counts as
catching a mutant. The normal verification run builds the unmodified projects.
"""

import argparse
import concurrent.futures as futures
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time

import leanfail

ROOT = Path(__file__).resolve().parents[2]

# app: the injected bug in words, for the `--json` record.
BUGS = {
    "kellnr": "a crate's last owner can be removed",
    "atuin": "the host filter on a user's records is dropped",
    "calculator": "a comparison is off by one (`<` becomes `<=`)",
    "board": "the last moderator can be removed",
    "ledger": "the owner check on a ledger action is removed",
    "inbox": "messages are readable by users outside the conversation",
    "booking": "the interval overlap check is off by one at the boundary",
    "wastebin": "the `secs == 0` branch of expiry handling is removed",
    "conduit": "a non-author may update an article",
    "cratesio": "expired invitations are still accepted",
    "filters": "a backslash is no longer escaped, only a quote",
    "keys": "an already-taken secret can be issued again",
}

# app: (path below examples/app, old source, injected source)
MUTANTS = {
    "kellnr": ("kellnr", "count_a(&s.owners, *krate) <= 1", "count_a(&s.owners, *krate) < 1"),
    "atuin": ("atuin", "&& r.host == host", "&& true"),
    "calculator": ("tutorials/calculator", "if a < b {", "if a <= b {"),
    "board": ("tutorials/board", "s.moderators.len() <= 1", "s.moderators.len() < 1"),
    "ledger": ("tutorials/ledger", "if a.owner != user {\n                Err(Error::Forbidden)\n            } else if l.deposited >", "if false {\n                Err(Error::Forbidden)\n            } else if l.deposited >"),
    "inbox": ("tutorials/inbox", "if from != user && to != user {", "if false {"),
    "booking": ("tutorials/booking", "start_at < v[i].end_at", "start_at <= v[i].end_at"),
    "wastebin": ("wastebin", "if secs == 0 {", "if false {"),
    "conduit": ("conduit", "if a.author != me.id {\n        return Err(Error::Forbidden);\n    }\n    if slug_taken", "if false {\n        return Err(Error::Forbidden);\n    }\n    if slug_taken"),
    "cratesio": ("cratesio", "if inv.expires <= p.now {", "if false {"),
    "filters": ("filters", "if *b == BSLASH || *b == QUOTE {", "if *b == QUOTE {"),
    "keys": ("keys", "if taken(snap, secret) {", "if false {"),
}

WORKSPACE = """[workspace]
resolver = "3"
members = ["APP_KERNEL", "crates/h5i-app-schema", "crates/h5i-app-sql"]

[workspace.package]
edition = "2024"
license = "Apache-2.0"
version = "0.1.0"
repository = "https://github.com/h5i-dev/h5i"
homepage = "https://github.com/h5i-dev/h5i"
readme = "README.md"

[workspace.dependencies]
h5i-app-schema = { path = "crates/h5i-app-schema" }
h5i-app-sql = { path = "crates/h5i-app-sql" }
"""


def run(args, cwd, log):
    with log.open("a") as stream:
        stream.write(f"$ {' '.join(map(str, args))}\n")
        stream.flush()
        return subprocess.run(args, cwd=cwd, stdout=stream, stderr=subprocess.STDOUT).returncode


def lake_failures(log, proofs):
    """Declarations the last `lake build` in `log` broke."""
    tail = log.read_text(errors="replace").rsplit("$ lake build", 1)[-1]
    return leanfail.failing_decls(tail, proofs)


def check(name, keep, baseline):
    """Returns (name, verdict, tmp, record); the record is the `--json` row."""
    app, old, new = MUTANTS[name]
    tmp = Path(tempfile.mkdtemp(prefix=f"h5i-app-mutant-{name}-"))
    log = tmp / "log.txt"
    rec = {"suite": "apps", "name": name, "app": f"examples/app/{app}", "bug": BUGS.get(name, ""), "expect": [],
           "verdict": "", "stage": "", "failed": []}
    t0 = time.time()
    try:
        src_app = ROOT / "examples" / "app" / app
        dst_app = tmp / "examples" / "app" / app
        dst_app.parent.mkdir(parents=True, exist_ok=True)
        shutil.copytree(src_app / "kernel", dst_app / "kernel", ignore=shutil.ignore_patterns("target"))
        for crate in ("h5i-app-schema", "h5i-app-sql"):
            dst = tmp / "crates" / crate
            dst.parent.mkdir(parents=True, exist_ok=True)
            shutil.copytree(ROOT / "crates" / crate, dst, ignore=shutil.ignore_patterns("target", "proofs"))
        (tmp / "Cargo.toml").write_text(WORKSPACE.replace("APP_KERNEL", f"examples/app/{app}/kernel"))

        kernel = dst_app / "kernel" / "src" / "lib.rs"
        if not baseline:
            source = kernel.read_text()
            count = source.count(old)
            if count != 1:
                rec.update(verdict="invalid", stage="pattern", secs=round(time.time() - t0, 1))
                return name, f"invalid pattern ({count} matches)", tmp, rec
            kernel.write_text(source.replace(old, new, 1))

        proofs = dst_app / "proofs"
        proofs.mkdir()
        for path in (src_app / "proofs").iterdir():
            if path.suffix == ".lean" or path.name in ("lakefile.lean", "lake-manifest.json", "lean-toolchain"):
                shutil.copy2(path, proofs / path.name)
        shutil.copytree(src_app / "proofs" / "generated", proofs / "generated")
        (proofs / ".lake").mkdir()
        (proofs / ".lake" / "packages").symlink_to(src_app / "proofs" / ".lake" / "packages")
        for filename in ("lakefile.lean", "lake-manifest.json"):
            file = proofs / filename
            file.write_text(file.read_text().replace(
                '../../../../../crates/h5i-app-core/proofs', str(ROOT / 'crates/h5i-app-core/proofs')).replace(
                '../../../../crates/h5i-app-core/proofs', str(ROOT / 'crates/h5i-app-core/proofs')))

        scripts = tmp / "scripts" / "app"
        scripts.mkdir(parents=True)
        extract = f"extract-{name}.sh"
        for script in (extract, "schema-items.sh", "normalize-sources.sh"):
            shutil.copy2(ROOT / "scripts" / "app" / script, scripts / script)

        if run(["cargo", "check", "-q"], tmp, log):
            verdict, stage, json_verdict = "invalid (rust)", "rust", "invalid"
        elif run(["bash", scripts / extract], tmp, log):
            verdict, stage, json_verdict = "invalid (extraction)", "extraction", "invalid"
        elif run(["lake", "build"], proofs, log):
            rec["failed"] = lake_failures(log, proofs)
            verdict, stage, json_verdict = ("BASELINE FAILED", "lake", "baseline-failed") if baseline else ("caught", "lake", "caught")
        else:
            verdict, stage, json_verdict = ("ok", "lake", "baseline-ok") if baseline else ("SURVIVED", "lake", "survived")
        rec.update(verdict=json_verdict, stage=stage, secs=round(time.time() - t0, 1))
        return name, verdict, tmp, rec
    finally:
        if not keep:
            shutil.rmtree(tmp, ignore_errors=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("-j", type=int, default=1, help="parallel mutants (large Lean builds need memory)")
    parser.add_argument("--baseline", action="store_true", help="check that unmodified kernels build in isolation")
    parser.add_argument("--keep", action="store_true", help="keep logs and temporary workspaces")
    parser.add_argument("--json", metavar="FILE", help="write one record per app")
    parser.add_argument("names", nargs="*", help="apps to check (default: all)")
    args = parser.parse_args()
    names = args.names or list(MUTANTS)
    unknown = set(names) - MUTANTS.keys()
    if unknown:
        parser.error(f"unknown apps: {', '.join(sorted(unknown))}")
    results = {}
    records = {}
    with futures.ThreadPoolExecutor(max_workers=args.j) as pool:
        pending = {pool.submit(check, name, args.keep, args.baseline): name for name in names}
        for task in futures.as_completed(pending):
            name = pending[task]
            try:
                _, verdict, tmp, rec = task.result()
            except Exception as error:  # noqa: BLE001
                verdict, tmp = f"error: {error}", None
                rec = {"suite": "apps", "name": name, "app": f"examples/app/{MUTANTS[name][0]}", "bug": BUGS.get(name, ""),
                       "expect": [], "verdict": "error", "stage": "", "failed": [], "error": str(error)}
            results[name] = verdict
            records[name] = rec
            print(f"{name:12} {verdict}" + (f"  ({tmp})" if args.keep and tmp else ""), flush=True)
    expected = "ok" if args.baseline else "caught"
    passed = sum(verdict == expected for verdict in results.values())
    print(f"{passed}/{len(names)} {'baselines built' if args.baseline else 'mutants caught'}")
    if args.json:
        with open(args.json, "w") as f:
            json.dump([records[n] for n in names], f, indent=1)
            f.write("\n")
    return 0 if passed == len(names) else 1


if __name__ == "__main__":
    sys.exit(main())
