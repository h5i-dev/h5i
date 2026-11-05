//! The assumption ledger: the stable IDs (A1 to A11) that code, reports and
//! docs cite, with their one-line names.
//!
//! This table is the source of truth. The ledger in
//! `docs/design/design-app-roadmap.md` lists the same IDs and names in its
//! first two columns, and `docs/design/design-app-trust.md` uses only these
//! IDs; the tests below fail when either drifts.

/// One trusted assumption.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Assumption {
    /// `A1` to `A11`; never reused or renumbered.
    pub id: &'static str,
    /// The roadmap ledger's "Assumption" column, verbatim.
    pub name: &'static str,
}

pub const ASSUMPTIONS: &[Assumption] = &[
    Assumption { id: "A1", name: "Authenticator returns the real sender" },
    Assumption { id: "A2", name: "JSON decoder is faithful" },
    Assumption { id: "A3", name: "Reply encoder adds nothing" },
    Assumption { id: "A4", name: "Postgres store matches `apply`" },
    Assumption { id: "A5", name: "SERIALIZABLE equals a serial order" },
    Assumption { id: "A6", name: "Engine protocol (retry, idempotency, lock, clock, outbox) is correct" },
    Assumption { id: "A7", name: "Charon, Aeneas, Lean are sound" },
    Assumption { id: "A8", name: "No handler bypasses the engine" },
    Assumption { id: "A9", name: "Running code is the extracted code" },
    Assumption { id: "A10", name: "The spec says what we meant" },
    Assumption {
        id: "A11",
        name: "An `[authz]` app's `Policy` (with `stored`), `Disclose` and `View` say what we meant",
    },
];

/// The assumption with this ID.
pub fn assumption(id: &str) -> Option<&'static Assumption> {
    ASSUMPTIONS.iter().find(|a| a.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn doc(name: &str) -> String {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/design").join(name);
        std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
    }

    /// `(id, second column)` of every table row whose first cell is an ID.
    fn rows(text: &str) -> Vec<(String, String)> {
        text.lines()
            .filter_map(|l| {
                let cells: Vec<&str> = l.trim().strip_prefix('|')?.split(" | ").map(str::trim).collect();
                let id = cells.first()?.trim_start_matches('|').trim();
                let numbered = id.strip_prefix('A').is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
                numbered.then(|| (id.to_string(), cells.get(1).copied().unwrap_or("").to_string()))
            })
            .collect()
    }

    #[test]
    fn the_roadmap_ledger_is_this_table() {
        let got = rows(&doc("design-app-roadmap.md"));
        let want: Vec<(String, String)> = ASSUMPTIONS.iter().map(|a| (a.id.to_string(), a.name.to_string())).collect();
        assert_eq!(got, want, "docs/design/design-app-roadmap.md's ledger differs from h5i-app-report's ASSUMPTIONS");
    }

    #[test]
    fn the_trust_model_cites_only_these_ids() {
        let text = doc("design-app-trust.md");
        let rows = rows(&text);
        assert!(!rows.is_empty());
        for (id, _) in rows {
            assert!(assumption(&id).is_some(), "design-app-trust.md has an assumption {id} the ledger does not");
        }
        for (i, _) in text.match_indices("(A") {
            let id: String = text[i + 1..].chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
            if id.len() > 1 && id[1..].chars().all(|c| c.is_ascii_digit()) {
                assert!(assumption(&id).is_some(), "design-app-trust.md cites ({id}), which the ledger does not have");
            }
        }
    }

    #[test]
    fn ids_are_distinct_and_numbered_in_order() {
        for (i, a) in ASSUMPTIONS.iter().enumerate() {
            assert_eq!(a.id, format!("A{}", i + 1));
        }
    }
}
