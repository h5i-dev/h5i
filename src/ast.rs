use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};

/// Represents a specific type of structural change in the AST.
#[derive(Debug, PartialEq, Clone)]
pub enum AstChange {
    /// A new structural element was added.
    Added { content: String },
    /// An existing structural element was removed.
    Deleted { content: String },
    /// A structural element was moved from one location to another without modification.
    Moved {
        content: String,
        old_index: usize,
        new_index: usize,
    },
    /// A structural element remains unchanged in the same relative position.
    Unchanged { content: String },
}

/// Result of a structural comparison between two ASTs.
pub struct AstDiff {
    pub changes: Vec<AstChange>,
    /// Percentage of similarity between 0.0 and 1.0.
    pub similarity: f32,
}

pub struct SemanticAst {
    pub raw_sexp: String,
    pub structure_hash: String,
}

impl SemanticAst {
    /// Creates a new SemanticAst from an S-expression string.
    /// It automatically computes a global structure hash.
    pub fn from_sexp(sexp: &str) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(sexp.as_bytes());
        let structure_hash = format!("{:x}", hasher.finalize());

        SemanticAst {
            raw_sexp: sexp.to_string(),
            structure_hash,
        }
    }

    /// Compares this AST with another and detects additions, deletions, and moves.
    /// This implementation treats top-level S-expressions as logical units.
    pub fn diff(&self, other: &Self) -> AstDiff {
        let base_blocks = self.parse_top_level_blocks();
        let head_blocks = other.parse_top_level_blocks();

        let mut changes = Vec::new();
        let mut base_map: HashMap<String, Vec<usize>> = HashMap::new();
        let mut head_map: HashMap<String, Vec<usize>> = HashMap::new();

        // Map blocks to their indices for move detection
        for (i, block) in base_blocks.iter().enumerate() {
            base_map
                .entry(self.hash_content(block))
                .or_default()
                .push(i);
        }
        for (i, block) in head_blocks.iter().enumerate() {
            head_map
                .entry(self.hash_content(block))
                .or_default()
                .push(i);
        }

        let mut base_processed = HashSet::new();
        let mut head_processed = HashSet::new();

        // 1. Detect Unchanged and Moved blocks
        for (new_idx, new_block) in head_blocks.iter().enumerate() {
            let h = self.hash_content(new_block);
            if let Some(old_indices) = base_map.get(&h) {
                // Find an unprocessed matching block from the base
                if let Some(&old_idx) = old_indices.iter().find(|&&i| !base_processed.contains(&i))
                {
                    if old_idx == new_idx {
                        changes.push(AstChange::Unchanged {
                            content: new_block.clone(),
                        });
                    } else {
                        changes.push(AstChange::Moved {
                            content: new_block.clone(),
                            old_index: old_idx,
                            new_index: new_idx,
                        });
                    }
                    base_processed.insert(old_idx);
                    head_processed.insert(new_idx);
                }
            }
        }

        // 2. Detect Deleted blocks
        for (old_idx, old_block) in base_blocks.iter().enumerate() {
            if !base_processed.contains(&old_idx) {
                changes.push(AstChange::Deleted {
                    content: old_block.clone(),
                });
            }
        }

        // 3. Detect Added blocks
        for (new_idx, new_block) in head_blocks.iter().enumerate() {
            if !head_processed.contains(&new_idx) {
                changes.push(AstChange::Added {
                    content: new_block.clone(),
                });
            }
        }

        let unchanged_count = changes
            .iter()
            .filter(|c| matches!(c, AstChange::Unchanged { .. }))
            .count();
        let similarity = if base_blocks.is_empty() && head_blocks.is_empty() {
            1.0
        } else {
            unchanged_count as f32 / base_blocks.len().max(head_blocks.len()) as f32
        };

        AstDiff {
            changes,
            similarity,
        }
    }

    /// Internal helper to split raw S-expressions into logical units.
    fn parse_top_level_blocks(&self) -> Vec<String> {
        self.raw_sexp
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect()
    }

    /// Internal helper to hash a single code block.
    fn hash_content(&self, content: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(content.as_bytes());
        format!("{:x}", hasher.finalize())
    }
}

impl AstDiff {
    /// Prints the diff to the console with stylish terminal colors.
    pub fn print_stylish(&self) {
        println!(
            "\x1b[1m\x1b[35m✨ H5i Structural Diff (Similarity: {:.1}%)\x1b[0m",
            self.similarity * 100.0
        );
        println!("\x1b[90m--------------------------------------------------\x1b[0m");

        for change in &self.changes {
            match change {
                AstChange::Added { content } => {
                    println!("\x1b[32m  + [Added]   {}\x1b[0m", content);
                }
                AstChange::Deleted { content } => {
                    println!("\x1b[31m  - [Deleted] {}\x1b[0m", content);
                }
                AstChange::Moved {
                    content,
                    old_index,
                    new_index,
                } => {
                    println!(
                        "\x1b[34m  m [Moved]   {} (L{} -> L{})\x1b[0m",
                        content,
                        old_index + 1,
                        new_index + 1
                    );
                }
                AstChange::Unchanged { content } => {
                    println!("\x1b[90m    [Fixed]   {}\x1b[0m", content);
                }
            }
        }
        println!("\x1b[90m--------------------------------------------------\x1b[0m");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ast_hash_consistency() {
        let ast1 = SemanticAst::from_sexp("(module (fn main))");
        let ast2 = SemanticAst::from_sexp("(module (fn main))");
        assert_eq!(ast1.structure_hash, ast2.structure_hash);
    }

    #[test]
    fn test_diff_detect_addition() {
        let base = SemanticAst::from_sexp("(fn a)");
        let head = SemanticAst::from_sexp("(fn a)\n(fn b)");
        let diff = base.diff(&head);

        assert!(diff
            .changes
            .iter()
            .any(|c| matches!(c, AstChange::Added { .. })));
        assert_eq!(diff.similarity, 0.5);
    }

    #[test]
    fn test_diff_detect_move() {
        // Swapping two functions
        let base = SemanticAst::from_sexp("(fn a)\n(fn b)");
        let head = SemanticAst::from_sexp("(fn b)\n(fn a)");
        let diff = base.diff(&head);

        let move_count = diff
            .changes
            .iter()
            .filter(|c| matches!(c, AstChange::Moved { .. }))
            .count();
        // Since both changed positions relative to the other, they are both 'Moved'
        assert_eq!(move_count, 2);
    }

    #[test]
    fn test_diff_empty_ast() {
        let base = SemanticAst::from_sexp("");
        let head = SemanticAst::from_sexp("");
        let diff = base.diff(&head);
        assert_eq!(diff.similarity, 1.0);
        assert!(diff.changes.is_empty());
    }

    #[test]
    fn test_diff_complete_change() {
        let base = SemanticAst::from_sexp("(fn a)");
        let head = SemanticAst::from_sexp("(fn b)");
        let diff = base.diff(&head);

        assert!(diff
            .changes
            .iter()
            .any(|c| matches!(c, AstChange::Deleted { .. })));
        assert!(diff
            .changes
            .iter()
            .any(|c| matches!(c, AstChange::Added { .. })));
        assert_eq!(diff.similarity, 0.0);
    }
}
