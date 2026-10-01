#!/usr/bin/env python3
"""Re-extract each non-docs kernel with one plausible bug and rebuild its proofs.

Rust compilation and extraction must succeed before a proof failure counts as
catching a mutant. The normal verification run builds the unmodified projects.
"""

import argparse
import concurrent.futures as futures
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[2]

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


def check(name, keep, baseline):
    app, old, new = MUTANTS[name]
    tmp = Path(tempfile.mkdtemp(prefix=f"h5i-app-mutant-{name}-"))
    log = tmp / "log.txt"
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
                return name, f"invalid pattern ({count} matches)", tmp
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
            verdict = "invalid (rust)"
        elif run(["bash", scripts / extract], tmp, log):
            verdict = "invalid (extraction)"
        elif run(["lake", "build"], proofs, log):
            verdict = "BASELINE FAILED" if baseline else "caught"
        else:
            verdict = "ok" if baseline else "SURVIVED"
        return name, verdict, tmp
    finally:
        if not keep:
            shutil.rmtree(tmp, ignore_errors=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("-j", type=int, default=1, help="parallel mutants (large Lean builds need memory)")
    parser.add_argument("--baseline", action="store_true", help="check that unmodified kernels build in isolation")
    parser.add_argument("--keep", action="store_true", help="keep logs and temporary workspaces")
    parser.add_argument("names", nargs="*", help="apps to check (default: all)")
    args = parser.parse_args()
    names = args.names or list(MUTANTS)
    unknown = set(names) - MUTANTS.keys()
    if unknown:
        parser.error(f"unknown apps: {', '.join(sorted(unknown))}")
    results = {}
    with futures.ThreadPoolExecutor(max_workers=args.j) as pool:
        pending = {pool.submit(check, name, args.keep, args.baseline): name for name in names}
        for task in futures.as_completed(pending):
            name = pending[task]
            try:
                _, verdict, tmp = task.result()
            except Exception as error:  # noqa: BLE001
                verdict, tmp = f"error: {error}", None
            results[name] = verdict
            print(f"{name:12} {verdict}" + (f"  ({tmp})" if args.keep and tmp else ""), flush=True)
    expected = "ok" if args.baseline else "caught"
    passed = sum(verdict == expected for verdict in results.values())
    print(f"{passed}/{len(names)} {'baselines built' if args.baseline else 'mutants caught'}")
    return 0 if passed == len(names) else 1


if __name__ == "__main__":
    sys.exit(main())
