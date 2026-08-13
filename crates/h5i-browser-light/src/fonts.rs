//! Fonts, found at runtime rather than linked at build time.
//!
//! Blitz can use fontconfig to enumerate system fonts, but that pulls
//! `yeslogic-fontconfig-sys`, which needs libfontconfig headers *when the
//! engine is compiled*. For something meant to build anywhere and run inside a
//! box, trading a build-time native dependency for a font list is a bad deal:
//! it breaks CI on a host without the dev package and it makes the render
//! depend on whatever the host happens to have installed.
//!
//! So fonts are discovered by walking directories at startup and registered
//! into parley directly. Two consequences worth knowing:
//!
//! - **A box with no fonts renders no text**, and that is a state the engine
//!   reports ([`FontSetup::is_empty`]) rather than a blank screenshot nobody
//!   can explain.
//! - **Generic families must be mapped explicitly.** With system fonts off,
//!   `font-family: sans-serif` resolves to nothing at all unless something
//!   points it at a real family, which is the difference between a page of
//!   text and a page of nothing.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use parley::fontique::{Blob, Collection, CollectionOptions, GenericFamily, SourceCache};
use parley::FontContext;

/// How many font files to register before stopping.
///
/// A full font directory can hold hundreds of files; registering all of them
/// costs startup time for coverage a docs page never uses. The cap is high
/// enough for the usual Latin + mono + a few fallbacks, and `--font-file`
/// exists for the case where the right font is not among them.
const DEFAULT_LIMIT: usize = 24;

const FONT_EXTENSIONS: [&str; 4] = ["ttf", "otf", "ttc", "otc"];

/// A ready font context plus what went into it.
pub struct FontSetup {
    pub context: FontContext,
    /// Files actually registered, in the order they were registered.
    pub sources: Vec<PathBuf>,
    /// Distinct families registered.
    pub families: usize,
}

impl FontSetup {
    pub fn is_empty(&self) -> bool {
        self.families == 0
    }

    /// A one-line summary for `doctor` and for the CLI's stderr note.
    pub fn summary(&self) -> String {
        if self.is_empty() {
            return "no fonts registered — text will not render".to_string();
        }
        format!(
            "{} font file(s), {} famil{}",
            self.sources.len(),
            self.families,
            if self.families == 1 { "y" } else { "ies" }
        )
    }
}

/// Directories to search when none are given.
///
/// Ordered by specificity: a user's own fonts win over the system's, because
/// someone who dropped a font in `~/.fonts` meant it.
pub fn default_font_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        dirs.push(home.join(".fonts"));
        dirs.push(home.join(".local/share/fonts"));
        if cfg!(target_os = "macos") {
            dirs.push(home.join("Library/Fonts"));
        }
    }

    if cfg!(target_os = "macos") {
        dirs.push(PathBuf::from("/Library/Fonts"));
        dirs.push(PathBuf::from("/System/Library/Fonts"));
    } else {
        dirs.push(PathBuf::from("/usr/local/share/fonts"));
        dirs.push(PathBuf::from("/usr/share/fonts"));
    }

    dirs
}

/// Score a font file by how much we want it as a general-purpose default.
///
/// Lower is better. This is a preference order, not a correctness rule: it
/// exists so that a 24-file budget spent on a directory of 800 fonts lands on
/// a usable sans/serif/mono set instead of the first 24 alphabetically (which
/// on a Debian host is a run of CJK and decorative faces).
fn preference_rank(path: &Path) -> u32 {
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    let preferred = [
        "dejavusans",
        "liberationsans",
        "notosans-regular",
        "arial",
        "helvetica",
        "dejavuserif",
        "liberationserif",
        "notoserif-regular",
        "times",
        "dejavusansmono",
        "liberationmono",
        "notosansmono-regular",
        "couriernew",
    ];

    for (index, candidate) in preferred.iter().enumerate() {
        if name == *candidate {
            return index as u32;
        }
    }
    for (index, candidate) in preferred.iter().enumerate() {
        if name.starts_with(candidate) {
            return 100 + index as u32;
        }
    }
    // Bold/italic variants of anything are useful, but only after a regular
    // face exists to pair them with.
    if name.contains("sans") || name.contains("serif") || name.contains("mono") {
        return 500;
    }
    1000
}

/// Walk a directory tree collecting font files. Depth-limited because a font
/// directory is not supposed to be deep, and an unbounded walk of a symlinked
/// tree is a way to hang at startup.
fn collect_font_files(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > 4 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_font_files(&path, depth + 1, out);
        } else if let Some(ext) = path.extension().and_then(|e| e.to_str())
            && FONT_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str())
        {
            out.push(path);
        }
    }
}

/// Build a font context from explicit files first, then whatever the search
/// directories turn up.
///
/// `explicit` files are always registered and never subject to the cap: if a
/// caller named a file, refusing it because a budget ran out would be the
/// tool second-guessing an instruction.
pub fn load(explicit: &[PathBuf], dirs: &[PathBuf], limit: Option<usize>) -> FontSetup {
    let limit = limit.unwrap_or(DEFAULT_LIMIT);

    let mut discovered = Vec::new();
    for dir in dirs {
        collect_font_files(dir, 0, &mut discovered);
    }
    discovered.sort_by_key(|path| (preference_rank(path), path.clone()));

    let mut chosen: Vec<PathBuf> = Vec::new();
    for path in explicit {
        if !chosen.contains(path) {
            chosen.push(path.clone());
        }
    }
    for path in discovered {
        if chosen.len() >= limit + explicit.len() {
            break;
        }
        if !chosen.contains(&path) {
            chosen.push(path);
        }
    }

    let mut collection = Collection::new(CollectionOptions {
        shared: false,
        system_fonts: false,
    });

    let mut families = Vec::new();
    let mut registered_paths = Vec::new();
    for path in chosen {
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let blob = Blob::new(Arc::new(bytes) as Arc<dyn AsRef<[u8]> + Send + Sync>);
        let added = collection.register_fonts(blob, None);
        if added.is_empty() {
            continue;
        }
        for (family_id, _) in added {
            if !families.contains(&family_id) {
                families.push(family_id);
            }
        }
        registered_paths.push(path);
    }

    // Without this, every `font-family: sans-serif` in every stylesheet
    // resolves to nothing and the page renders empty despite the fonts being
    // present and registered.
    for generic in [
        GenericFamily::SansSerif,
        GenericFamily::Serif,
        GenericFamily::Monospace,
        GenericFamily::SystemUi,
        GenericFamily::UiSansSerif,
        GenericFamily::UiSerif,
        GenericFamily::UiMonospace,
    ] {
        collection.set_generic_families(generic, families.iter().copied());
    }

    FontSetup {
        context: FontContext {
            collection,
            source_cache: SourceCache::new_shared(),
        },
        families: families.len(),
        sources: registered_paths,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_fonts_is_a_reportable_state_not_a_panic() {
        let setup = load(&[], &[PathBuf::from("/nonexistent-font-dir")], None);
        assert!(setup.is_empty());
        assert!(setup.summary().contains("no fonts"));
    }

    #[test]
    fn preference_puts_a_general_purpose_sans_ahead_of_a_decorative_face() {
        let sans = preference_rank(Path::new("/usr/share/fonts/DejaVuSans.ttf"));
        let cjk = preference_rank(Path::new("/usr/share/fonts/ukai.ttc"));
        assert!(
            sans < cjk,
            "a 24-file budget must not be spent alphabetically"
        );
    }

    #[test]
    fn bold_variants_rank_below_their_regular_face() {
        let regular = preference_rank(Path::new("DejaVuSans.ttf"));
        let bold = preference_rank(Path::new("DejaVuSans-Bold.ttf"));
        assert!(regular < bold);
    }

    #[test]
    fn only_font_extensions_are_collected() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("real.ttf"), b"not really a font").unwrap();
        std::fs::write(dir.path().join("notes.txt"), b"text").unwrap();
        std::fs::write(dir.path().join("styles.css"), b"css").unwrap();

        let mut found = Vec::new();
        collect_font_files(dir.path(), 0, &mut found);

        assert_eq!(found.len(), 1);
        assert!(found[0].ends_with("real.ttf"));
    }

    #[test]
    fn a_file_that_is_not_a_font_is_skipped_rather_than_counted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let fake = dir.path().join("fake.ttf");
        std::fs::write(&fake, b"this is not a font").unwrap();

        let setup = load(&[fake], &[], None);
        // It was named explicitly, but it registers no families, so it must
        // not inflate the count that `doctor` reports.
        assert!(setup.is_empty());
        assert!(setup.sources.is_empty());
    }
}
