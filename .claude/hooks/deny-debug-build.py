#!/usr/bin/env python3
"""PreToolUse(Bash) guard: refuse anything that would compile into target/debug.

A dev-profile build of this workspace is very large. On a machine that cannot
spare the space, this hook pins the checkout to release-only: it stops the
command before it runs and says what to type instead. The hard layer is the
placeholder file at target/debug (scripts/no-debug-guard.sh), which stops every
other tool too.

Opt-in, and deliberately not wired up by the repository. Nothing here fires
until someone adds it to their own .claude/settings.local.json, because whether
a dev build is affordable is a fact about the machine, not about the project.
See CLAUDE.md.
"""
import json
import re
import sys

# cargo subcommands that codegen (or check) into target/debug without --release.
BUILD_VERBS = {
    "build", "b", "test", "t", "run", "r", "bench", "check", "c", "clippy",
    "doc", "d", "rustc", "rustdoc", "nextest", "llvm-cov", "miri", "tarpaulin",
    "fuzz", "careful", "expand", "udeps",
}
RELEASE_FLAGS = {"--release", "-r", "--profile=release"}
# Spelled indirectly so this file can be edited by a shell command that the
# hook itself is inspecting.
TARGET_DIR_ENV = "CARGO_" + "TARGET_DIR"
SEGMENT_SPLIT = re.compile(r"(?:\|\||&&|[;|&\n(]|\$\()")
ENV_ASSIGN = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*=")


def deny(reason):
    json.dump({"hookSpecificOutput": {
        "hookEventName": "PreToolUse",
        "permissionDecision": "deny",
        "permissionDecisionReason": reason,
    }}, sys.stdout)
    sys.exit(0)


def cargo_args(segment):
    """Args of `cargo ...` when the segment actually starts with cargo, else None."""
    tokens = segment.split()
    while tokens and ENV_ASSIGN.match(tokens[0]):
        tokens.pop(0)
    if not tokens or tokens[0].split("/")[-1] not in ("cargo", "cargo.exe"):
        return None
    return [t for t in tokens[1:] if not t.startswith("+")]


def main():
    try:
        command = json.load(sys.stdin).get("tool_input", {}).get("command", "")
    except (json.JSONDecodeError, AttributeError):
        sys.exit(0)

    for segment in SEGMENT_SPLIT.split(command):
        # Only a segment that actually runs the command counts. Text that
        # merely mentions the guard (a heredoc, a commit message) is not
        # tampering, and blocking it would make the guard unmaintainable.
        if re.match(r"\s*(?:sudo\s+)?(?:rm|mv|rmdir|unlink|truncate|chattr)\b[^|;&]*target/debug",
                    segment) or re.match(r"\s*\S*no-debug-guard\.sh\s+off", segment):
            deny("target/debug is guarded on purpose: this checkout is "
                 "release-only. Removing the guard is the owner's decision, not "
                 "yours. Build with --release instead.")

        args = cargo_args(segment)
        if args is None:
            continue
        if re.match(r"\s*(?:[A-Za-z_][A-Za-z0-9_]*=\S*\s+)*" + TARGET_DIR_ENV + "=", segment) or \
                any(a == "--target-dir" or a.startswith("--target-dir=") for a in args):
            deny("Redirecting the cargo target dir just puts the dev-profile "
                 "artifacts elsewhere. Build into target/release instead.")
        verb = next((a for a in args if not a.startswith("-")), None)
        if verb not in BUILD_VERBS:
            continue
        if any(a in RELEASE_FLAGS for a in args):
            continue
        if "--profile" in args:
            i = args.index("--profile")
            profile = args[i + 1] if i + 1 < len(args) else ""
            if profile == "release":
                continue
            deny(f"Profile '{profile}' is not the release profile. This "
                 "checkout is release-only. Use --release.")
        deny(f"`cargo {verb}` without --release compiles into target/debug, "
             "which this checkout does not allow. Run "
             f"`cargo {verb} --release ...` instead (target/release is already "
             "warm, so it is also the faster path).")


if __name__ == "__main__":
    main()
