//! The Spec Kit preset (`presets/speckit/`), carried by the binary so it
//! matches the `h5i app` formats it describes.

use std::path::{Path, PathBuf};

use crate::error::H5iError;
use crate::skill::Page;

/// Every file of the preset, relative to the preset root.
pub const FILES: &[Page] = &[
    Page {
        path: "preset.yml",
        text: include_str!("../../../presets/speckit/preset.yml"),
    },
    Page {
        path: "README.md",
        text: include_str!("../../../presets/speckit/README.md"),
    },
    Page {
        path: "templates/constitution-addendum.md",
        text: include_str!("../../../presets/speckit/templates/constitution-addendum.md"),
    },
    Page {
        path: "templates/spec-addendum.md",
        text: include_str!("../../../presets/speckit/templates/spec-addendum.md"),
    },
    Page {
        path: "templates/plan-addendum.md",
        text: include_str!("../../../presets/speckit/templates/plan-addendum.md"),
    },
    Page {
        path: "templates/tasks-addendum.md",
        text: include_str!("../../../presets/speckit/templates/tasks-addendum.md"),
    },
    Page {
        path: "commands/speckit.plan.md",
        text: include_str!("../../../presets/speckit/commands/speckit.plan.md"),
    },
    Page {
        path: "commands/speckit.implement.md",
        text: include_str!("../../../presets/speckit/commands/speckit.implement.md"),
    },
    Page {
        path: "commands/speckit.h5i.trace.md",
        text: include_str!("../../../presets/speckit/commands/speckit.h5i.trace.md"),
    },
];

/// Where `export` writes by default, relative to the current directory.
pub const DEFAULT_DIR: &str = "h5i-speckit-preset";

/// `preset.yml` with the preset's version set to `version`, so
/// `specify preset list` shows which h5i the preset came from.
pub fn manifest(version: &str) -> String {
    let mut done = false;
    let mut out = String::new();
    for line in FILES[0].text.lines() {
        if !done && line.starts_with("  version: ") {
            out.push_str(&format!("  version: \"{version}\""));
            done = true;
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

/// Write the preset under `target`. Returns the paths written.
pub fn export(target: &Path, version: &str) -> Result<Vec<PathBuf>, H5iError> {
    let mut written = Vec::new();
    for file in FILES {
        let path = target.join(file.path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| H5iError::with_path(e, parent))?;
        }
        let text = if file.path == "preset.yml" {
            manifest(version)
        } else {
            file.text.to_string()
        };
        std::fs::write(&path, text).map_err(|e| H5iError::with_path(e, &path))?;
        written.push(path);
    }
    Ok(written)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Every regular file under `root`, as `/`-separated relative paths.
    pub(crate) fn files_under(root: &Path) -> Vec<String> {
        fn visit(dir: &Path, root: &Path, out: &mut Vec<String>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    visit(&path, root, out);
                } else {
                    let rel = path.strip_prefix(root).unwrap();
                    let parts: Vec<_> = rel.iter().map(|c| c.to_string_lossy()).collect();
                    out.push(parts.join("/"));
                }
            }
        }
        let mut out = Vec::new();
        visit(root, root, &mut out);
        out
    }

    #[test]
    fn every_preset_file_is_embedded() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../presets/speckit");
        for path in files_under(&root) {
            assert!(
                FILES.iter().any(|f| f.path == path),
                "presets/speckit/{path} is not in FILES"
            );
        }
    }

    #[test]
    fn every_declared_file_is_shipped() {
        // preset.yml's `file:` entries must all exist, or Spec Kit refuses it.
        for line in FILES[0].text.lines() {
            if let Some(rest) = line.trim().strip_prefix("file: ") {
                let path = rest.trim_matches('"');
                assert!(FILES.iter().any(|f| f.path == path), "{path} not shipped");
            }
        }
    }

    #[test]
    fn manifest_carries_the_h5i_version() {
        let text = manifest("9.8.7");
        assert!(text.contains("\n  version: \"9.8.7\"\n"), "{text}");
        let versions = text
            .lines()
            .filter(|l| l.starts_with("  version: "))
            .count();
        assert_eq!(versions, 1, "{text}");
        assert!(text.contains("schema_version: \"1.0\""));
        assert!(text.contains("speckit_version: \">=0.9.0\""));
    }

    #[test]
    fn export_writes_every_file() {
        let td = TempDir::new().unwrap();
        let written = export(td.path(), "1.2.3").unwrap();
        assert_eq!(written.len(), FILES.len());
        for f in &FILES[1..] {
            assert_eq!(
                std::fs::read_to_string(td.path().join(f.path)).unwrap(),
                f.text
            );
        }
        let yml = std::fs::read_to_string(td.path().join("preset.yml")).unwrap();
        assert!(yml.contains("version: \"1.2.3\""));
    }
}
