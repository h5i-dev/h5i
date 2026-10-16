//! Small CPU/device-shared search and push-rule decisions. No allocation in
//! decisions; bounded flat slices are supplied by the host's schema adapter.
#[cfg(not(target_arch = "nvptx64"))]
h5i_app_schema::schema! {
    mapping parallel_tables for parallel_kernel, lean "../proofs/generated/Schema.lean";
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct Snapshot { bookmarks: Vec<Bookmark>, events: Vec<Event>, rules: Vec<Rule> }
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Bookmark in "bookmarks" { key { id: u64 } text: Vec<u8>, tags: u64, }
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Event in "events" { key { id: u64 } content: Vec<u8>, display: Vec<u8>, members: u64, power: u64, }
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Rule in "rules" { key { id: u64 } pattern: Vec<u8>, minimum: u64, maximum: u64, required: u64, enabled: bool, display: bool, }
}

/// Included in extraction so the schema generator can check its clone laws.
#[cfg(not(target_arch = "nvptx64"))]
pub fn clone_snapshot(s: &Snapshot) -> Snapshot { s.clone() }

pub fn fold(c: u8) -> u8 { if c >= 65 && c <= 90 { c + 32 } else { c } }

/// ASCII substring in a bounded range. Empty needles match; malformed ranges
/// fail closed. Zero padding separates title/description/url/notes fields.
pub fn contains(text: &[u8], query: &[u8], start: usize, len: usize) -> bool {
    if start > query.len() || len > query.len() - start { return false; }
    if len > text.len() { return false; }
    let mut i = 0;
    while i <= text.len() - len {
        let mut j = 0;
        let mut same = true;
        while j < len {
            if fold(text[i+j]) != fold(query[start+j]) { same = false; }
            j += 1;
        }
        if same { return true; }
        i += 1;
    }
    false
}

/// Up to eight 16-byte term slots, padded with zero; per-term NOT and a choice
/// of AND/OR. Tags are a toy 64-tag dictionary: every required tag must exist.
pub fn bookmark_search(text: &[u8], terms: &[u8], count: usize, any: bool, negated: u32, tags: u64, required: u64) -> bool {
    if tags & required != required { return false; }
    if count > 8 || terms.len() != 128 { return false; }
    let mut matched = !any;
    let mut t = 0;
    while t < count {
        let start = t * 16;
        let mut len = 0;
        while len < 16 && terms[start+len] != 0 { len += 1; }
        if len == 0 { return false; }
        let found = contains(text, terms, start, len);
        let hit = if negated & (1 << t) != 0 { !found } else { found };
        if any { matched = matched || hit; } else { matched = matched && hit; }
        t += 1;
    }
    matched
}

/// Case-sensitive glob with '*' and '?', using a fixed-capacity dynamic program (patterns up to 64 bytes).
/// Padding after the first zero is ignored. This is a toy byte matcher, not a
/// claim of complete Matrix Unicode/word-boundary semantics.
pub fn glob(text: &[u8], pattern: &[u8]) -> bool {
    let mut nt = 0;
    while nt < text.len() && text[nt] != 0 { nt += 1; }
    let mut np = 0;
    while np < pattern.len() && pattern[np] != 0 { np += 1; }
    if np > 64 { return false; }
    let mut prev = [false; 65];
    prev[0] = true;
    let mut p = 1;
    while p <= np {
        prev[p] = prev[p-1] && pattern[p-1] == 42;
        p += 1;
    }
    let mut t = 0;
    while t < nt {
        let mut next = [false; 65];
        let mut p = 1;
        while p <= np {
            if pattern[p-1] == 42 { next[p] = next[p-1] || prev[p]; }
            else { next[p] = prev[p-1] && (pattern[p-1] == 63 || pattern[p-1] == text[t]); }
            p += 1;
        }
        prev = next;
        t += 1;
    }
    prev[np]
}

/// One (event,rule) decision: glob, display name, member count and permission.
/// The first failing condition denies. Returns a scalar device-friendly tag.
pub fn push_rule(event: &[u8], pattern: &[u8], display: &[u8], members: u64, minimum: u64, maximum: u64, power: u64, required: u64, enabled: bool, require_display: bool) -> u32 {
    if !enabled { return 0; }
    if power < required { return 0; }
    if members < minimum || members > maximum { return 0; }
    if !glob(event,pattern) { return 0; }
    let mut len = 0;
    while len < display.len() && display[len] != 0 { len += 1; }
    if require_display && (len == 0 || !contains(event,display,0,len)) { return 0; }
    1
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn search_boolean_tags_and_case() {
        let mut terms=[0;128]; terms[..4].copy_from_slice(b"RUST"); terms[16..19].copy_from_slice(b"gpu");
        assert!(bookmark_search(b"Rust notes\0https://GPU.test",&terms,2,false,0,3,1));
        assert!(!bookmark_search(b"Rust",&terms,2,false,0,3,1));
        assert!(bookmark_search(b"Rust",&terms,2,true,0,3,1));
        assert!(bookmark_search(b"Rust",&terms,2,false,2,3,1));
        assert!(!bookmark_search(b"Rust GPU",&terms,2,false,0,0,1));
        assert!(!bookmark_search(b"Rust",&terms,9,false,0,3,1));
    }
    #[test]
    fn glob_and_push_conditions() {
        assert!(glob(b"hello Alice",b"h*?Alice"));
        assert!(!glob(b"hello",b"*Alice"));
        assert_eq!(push_rule(b"hello Alice",b"h*",b"Alice",5,2,10,50,20,true,true),1);
        assert_eq!(push_rule(b"hello Alice",b"h*",b"Alice",5,2,10,10,20,true,true),0);
        assert_eq!(push_rule(b"hello Alice",b"h*",b"Alice",5,2,10,50,20,false,true),0);
    }
    #[test]
    fn malformed_and_capacity_boundaries_fail_closed() {
        let empty_terms = [0;128];
        assert!(!bookmark_search(b"text", &empty_terms, 1, false, 0, 0, 0));
        assert!(!bookmark_search(b"text", &[0;127], 0, false, 0, 0, 0));
        assert!(!bookmark_search(b"text", &empty_terms, usize::MAX, false, 0, 0, 0));
        assert!(bookmark_search(b"", &empty_terms, 0, false, 0, u64::MAX, u64::MAX));
        assert!(glob(&[b'a';64], &[b'?';64]));
        assert!(!glob(&[b'a';65], &[b'?';65]));
        assert_eq!(push_rule(b"text", b"*", b"", 1, 0, 2, 1, 0, true, true), 0);
    }

}
