"""`python3 -m unittest scripts/app/leanfail_test.py` from the repository root."""

import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import leanfail  # noqa: E402

LOG = """$ lake build Theorems
✖ [18/20] Building Theorems
error: ././././Theorems.lean:6:2: unsolved goals
case h
⊢ False
Invariants.lean:9:13: error: omega could not prove the goal:
error: ././././Theorems.lean:6:2: unsolved goals
error: Lean exited with code 1
error: build failed
"""

SOURCE = """namespace k.T

/-- Never fails. -/
theorem total (s : S) :
    ∃ r, f s = .ok r := by
  simp

@[simp] theorem helper : True := trivial

end k.T

theorem loose : True := by
  trivial
"""


class LeanFail(unittest.TestCase):
    def test_failures_are_distinct_locations_in_order(self):
        self.assertEqual(
            leanfail.failures(LOG),
            [("Theorems.lean", 6, 2, "unsolved goals"), ("Invariants.lean", 9, 13, "omega could not prove the goal:")],
        )

    def test_enclosing_decl_is_namespace_qualified(self):
        self.assertEqual(leanfail.enclosing_decl(SOURCE, 6), ("theorem", "k.T.total"))
        self.assertEqual(leanfail.enclosing_decl(SOURCE, 8), ("theorem", "k.T.helper"))
        self.assertEqual(leanfail.enclosing_decl(SOURCE, 13), ("theorem", "loose"))
        self.assertIsNone(leanfail.enclosing_decl(SOURCE, 1))

    def test_failing_decls_reads_the_project(self):
        with tempfile.TemporaryDirectory() as d:
            with open(os.path.join(d, "Theorems.lean"), "w") as f:
                f.write(SOURCE)
            recs = leanfail.failing_decls(LOG, d)
            self.assertEqual(recs[0]["decl"], "k.T.total")
            self.assertEqual(recs[0]["kind"], "theorem")
            self.assertIsNone(recs[1]["decl"], "a file that is not there resolves to nothing")
            self.assertEqual(leanfail.decl_names(recs), ["k.T.total"])


if __name__ == "__main__":
    unittest.main()
