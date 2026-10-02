"""Which Lean declarations a failed `lake build` broke.

Lake prints `error: File.lean:LINE:COL: message` (older) or
`File.lean:LINE:COL: error: message`, with `./` prefixes on the path. The
enclosing declaration is found by reading the file back to the nearest
`theorem`/`lemma`/`def` head, qualified by the open namespaces.
"""

import re
from pathlib import Path

_ERROR = re.compile(
    r"^(?:error:\s*)?(?P<file>[\w./@+-]*?\.lean):(?P<line>\d+):(?P<col>\d+):\s*(?:error:\s*)?(?P<msg>.*)$"
)
_DECL = re.compile(
    r"^\s*(?:@\[[^\]]*\]\s*)*(?:(?:private|protected|noncomputable|nonrec|partial)\s+)*"
    r"(?P<kind>theorem|lemma|def|abbrev|instance|example|structure|inductive|opaque)\s+(?P<name>[\w.'!?]+)"
)
_NAMESPACE = re.compile(r"^\s*namespace\s+(?P<name>[\w.]+)")
_END = re.compile(r"^\s*end(?:\s+(?P<name>[\w.]+))?\s*$")


def failures(log_text):
    """Distinct `(file, line, col, message)` error locations, in order."""
    seen = set()
    out = []
    for raw in log_text.splitlines():
        line = raw.strip()
        if "error" not in line:
            continue
        m = _ERROR.match(line)
        if not m:
            continue
        file = re.sub(r"^(?:\./)+", "", m["file"])
        key = (file, int(m["line"]), int(m["col"]))
        if key in seen:
            continue
        seen.add(key)
        out.append((file, int(m["line"]), int(m["col"]), m["msg"].strip()))
    return out


def enclosing_decl(source, line):
    """`(kind, qualified name)` of the declaration containing 1-based `line`, or None."""
    lines = source.splitlines()
    stack = []
    decl = None
    for i, text in enumerate(lines[: max(line, 0)], start=1):
        if m := _NAMESPACE.match(text):
            stack.append(m["name"])
            decl = None
        elif m := _END.match(text):
            if stack and (m["name"] is None or stack[-1] == m["name"]):
                stack.pop()
            decl = None
        elif m := _DECL.match(text):
            prefix = ".".join(stack)
            name = m["name"]
            decl = (m["kind"], f"{prefix}.{name}" if prefix and not name.startswith(prefix) else name)
        if i == line:
            break
    return decl


def failing_decls(log_text, proofs_dir):
    """One record per error location, with the declaration resolved from `proofs_dir`."""
    proofs_dir = Path(proofs_dir)
    out = []
    for file, line, col, msg in failures(log_text):
        rec = {"file": file, "line": line, "col": col, "message": msg, "decl": None, "kind": None}
        path = proofs_dir / file
        if path.is_file():
            if found := enclosing_decl(path.read_text(errors="replace"), line):
                rec["kind"], rec["decl"] = found
        out.append(rec)
    return out


def decl_names(records):
    """Distinct declaration names from `failing_decls`, in order."""
    names = []
    for r in records:
        if r["decl"] and r["decl"] not in names:
            names.append(r["decl"])
    return names
