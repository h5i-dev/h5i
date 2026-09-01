//! Fonts, found at runtime rather than linked at build time.

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
    // An emoji face, ranked ahead of the weight and slant variants below.
    if name.contains("emoji") || name.contains("symbola") {
        return 50;
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

/// Whether a face carries glyph outlines, as opposed to only bitmaps.
fn has_outlines(bytes: &[u8]) -> bool {
    fn tables_at(bytes: &[u8], base: usize) -> Option<bool> {
        let count = u16::from_be_bytes(bytes.get(base + 4..base + 6)?.try_into().ok()?) as usize;
        let mut outline = false;
        let mut bitmap = false;
        for i in 0..count {
            let at = base + 12 + 16 * i;
            match bytes.get(at..at + 4)? {
                b"glyf" | b"CFF " | b"CFF2" => outline = true,
                b"CBDT" | b"sbix" | b"EBDT" => bitmap = true,
                _ => {}
            }
        }
        // Only a face that has bitmaps *and* no outlines is the problem case.
        // A face with neither is something else entirely and is left alone.
        Some(outline || !bitmap)
    }

    let Some(tag) = bytes.get(0..4) else {
        return true;
    };
    if tag == b"ttcf" {
        // A collection: judge it by its first face, which is the one a
        // single-family claim would come from anyway.
        let Some(raw) = bytes.get(12..16) else {
            return true;
        };
        let first = u32::from_be_bytes(raw.try_into().unwrap_or([0; 4])) as usize;
        return tables_at(bytes, first).unwrap_or(true);
    }
    tables_at(bytes, 0).unwrap_or(true)
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

    // Two lists, joined at the end: a face that cannot draw an outline goes
    // behind every face that can, whatever order it was named in. `--font-file`
    // still means "register this". It just no longer means "and let it capture
    // the digits". See `has_outlines`.
    let mut families = Vec::new();
    let mut bitmap_only = Vec::new();
    let mut emoji = Vec::new();
    let mut registered_paths = Vec::new();
    for path in chosen {
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let drawable = has_outlines(&bytes);
        let is_emoji = path
            .file_stem()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.to_ascii_lowercase().contains("emoji"));
        let blob = Blob::new(Arc::new(bytes) as Arc<dyn AsRef<[u8]> + Send + Sync>);
        let added = collection.register_fonts(blob, None);
        if added.is_empty() {
            continue;
        }
        let list = if drawable {
            &mut families
        } else {
            &mut bitmap_only
        };
        for (family_id, _) in added {
            if !list.contains(&family_id) {
                list.push(family_id);
            }
            if is_emoji && !emoji.contains(&family_id) {
                emoji.push(family_id);
            }
        }
        registered_paths.push(path);
    }
    families.retain(|id| !bitmap_only.contains(id));
    families.extend(bitmap_only.iter().copied());

    // Emoji is a query of its own, and it is the one that was going unanswered.
    let emoji_families: Vec<_> = emoji
        .iter()
        .filter(|id| families.contains(id) || bitmap_only.contains(id))
        .copied()
        .collect();
    if !emoji_families.is_empty() {
        collection.set_generic_families(GenericFamily::Emoji, emoji_families.into_iter());
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
    fn a_colour_bitmap_face_is_not_treated_as_drawable() {
        // The real thing, because the point of this check is a real font's real
        // table set. A synthetic header would only test the parser.
        let noto = Path::new("/usr/share/fonts/truetype/noto/NotoColorEmoji.ttf");
        let Ok(bytes) = std::fs::read(noto) else {
            // Not installed here. Skipping is right: this asserts about a file
            // the host may not have, and inventing one would assert nothing.
            return;
        };
        assert!(
            !has_outlines(&bytes),
            "NotoColorEmoji has CBDT and no glyf; letting it rank as drawable \
             puts it in front of the text faces, where it captures the digits \
             and the space and draws neither"
        );
    }

    #[test]
    fn an_ordinary_text_face_is_drawable() {
        for candidate in [
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
        ] {
            if let Ok(bytes) = std::fs::read(candidate) {
                assert!(has_outlines(&bytes), "{candidate} has outlines");
                return;
            }
        }
    }

    #[test]
    fn garbage_is_drawable_because_unsure_must_not_demote() {
        assert!(has_outlines(b""));
        assert!(has_outlines(b"not a font at all"));
    }

    #[test]
    fn an_emoji_face_outranks_the_weight_variants() {
        // The regression this pins is an off-by-one in a budget, not a
        // preference. Ranked after the DejaVu obliques, `NotoColorEmoji` came
        // twenty-fifth of 817 against a cap of twenty-four and was never
        // registered. A tofu box on every page, on a host that had the font.
        let emoji = preference_rank(Path::new("NotoColorEmoji.ttf"));
        let oblique = preference_rank(Path::new("DejaVuSans-Oblique.ttf"));
        let sans = preference_rank(Path::new("DejaVuSans.ttf"));
        assert!(
            emoji < oblique,
            "a synthesisable slant must not outrank the only cover for a range"
        );
        assert!(sans < emoji, "and it is still never the body text face");
    }

    #[test]
    fn the_default_scan_reaches_an_emoji_face() {
        // The end-to-end version of the rank test: whatever this host has, if
        // it has an emoji font at all then the real scan with the real cap must
        // register it. Ranking it correctly is worthless if the budget still
        // runs out first.
        let dirs = default_font_dirs();
        let mut all = Vec::new();
        for dir in &dirs {
            collect_font_files(dir, 0, &mut all);
        }
        let named = |p: &PathBuf| p.to_string_lossy().to_lowercase().contains("emoji");
        if !all.iter().any(named) {
            return; // no emoji font installed; nothing to assert
        }
        let setup = load(&[], &dirs, None);
        assert!(
            setup.sources.iter().any(named),
            "an emoji font is installed but the scan did not reach it: {:?}",
            setup.sources
        );
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
