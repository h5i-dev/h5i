//! Host layout adapter checked against the schema-generated Bookmark row.
//! Device code imports only pure decisions, never these allocating functions.
pub use crate::layout::{MAX_ROWS, ROW_BYTES, RULE_BYTES, TERMS_BYTES};
use parallel_kernel::Bookmark;
#[derive(Debug, PartialEq, Eq)]
pub enum LayoutError {
    Capacity,
    Text,
    EmbeddedZero,
}
pub struct SearchArena {
    pub ids: Vec<u64>,
    pub text: Vec<u8>,
    pub tags: Vec<u64>,
}
impl SearchArena {
    pub fn pack(rows: &[Bookmark], capacity: usize) -> Result<Self, LayoutError> {
        if rows.len() > capacity || capacity > MAX_ROWS {
            return Err(LayoutError::Capacity);
        }
        let mut a = Self {
            ids: Vec::with_capacity(rows.len()),
            text: vec![0; rows.len() * ROW_BYTES],
            tags: Vec::with_capacity(rows.len()),
        };
        for (i, r) in rows.iter().enumerate() {
            if r.text.len() > ROW_BYTES {
                return Err(LayoutError::Text);
            }
            // Zero field separators are permitted; trailing zero would lose length.
            if r.text.last() == Some(&0) {
                return Err(LayoutError::EmbeddedZero);
            }
            a.ids.push(r.id);
            a.tags.push(r.tags);
            a.text[i * ROW_BYTES..i * ROW_BYTES + r.text.len()].copy_from_slice(&r.text);
        }
        Ok(a)
    }
    pub fn unpack(&self) -> Vec<Bookmark> {
        self.text
            .chunks_exact(ROW_BYTES)
            .enumerate()
            .map(|(i, text)| {
                let len = text.iter().rposition(|b| *b != 0).map_or(0, |j| j + 1);
                Bookmark {
                    id: self.ids[i],
                    text: text[..len].to_vec(),
                    tags: self.tags[i],
                }
            })
            .collect()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capacity_roundtrip_and_input_order() {
        let rows = vec![
            Bookmark {
                id: 9,
                text: b"title\0url".to_vec(),
                tags: 3,
            },
            Bookmark {
                id: 1,
                text: vec![b'a'; ROW_BYTES],
                tags: 1,
            },
        ];
        let a = SearchArena::pack(&rows, 2).unwrap();
        assert_eq!(a.unpack(), rows);
        assert_eq!(a.ids, vec![9, 1]);
        assert_eq!(
            SearchArena::pack(&rows, 1).err(),
            Some(LayoutError::Capacity)
        );
        assert_eq!(
            SearchArena::pack(&[], MAX_ROWS + 1).err(),
            Some(LayoutError::Capacity)
        );
        let too_long = [Bookmark {
            id: 0,
            text: vec![1; 513],
            tags: 0,
        }];
        assert_eq!(
            SearchArena::pack(&too_long, 1).err(),
            Some(LayoutError::Text)
        );
        assert_eq!(SearchArena::pack(&[], 0).unwrap().unpack(), vec![]);
        assert!(SearchArena::pack(&[], MAX_ROWS).is_ok());
        let trailing_zero = [Bookmark {
            id: 0,
            text: vec![1, 0],
            tags: 0,
        }];
        assert_eq!(
            SearchArena::pack(&trailing_zero, 1).err(),
            Some(LayoutError::EmbeddedZero)
        );
    }
    #[test]
    fn batches_are_independent_on_cpu() {
        let terms = [b'a'; TERMS_BYTES];
        let rows = vec![
            Bookmark {
                id: 4,
                text: vec![b'a'; ROW_BYTES],
                tags: 1,
            },
            Bookmark {
                id: 2,
                text: b"no match".to_vec(),
                tags: 0,
            },
        ];
        let packed = SearchArena::pack(&rows, 2).unwrap();
        let evaluate = |a: &SearchArena| {
            a.text
                .chunks_exact(ROW_BYTES)
                .zip(&a.tags)
                .map(|(r, t)| parallel_kernel::bookmark_search(r, &terms, 1, false, 0, *t, 1))
                .collect::<Vec<_>>()
        };
        let together = evaluate(&packed);
        let separate = rows
            .iter()
            .flat_map(|r| evaluate(&SearchArena::pack(std::slice::from_ref(r), 1).unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(together, separate);
    }
}

/// Event SoA checked against schema-generated field types. A batch evaluates
/// these events against a supplied rule; ids stay in input order on the host.
pub struct PushArena {
    pub ids: Vec<u64>,
    pub content: Vec<u8>,
    pub display: Vec<u8>,
    pub members: Vec<u64>,
    pub powers: Vec<u64>,
}
impl PushArena {
    pub fn pack(rows: &[parallel_kernel::Event], capacity: usize) -> Result<Self, LayoutError> {
        if rows.len() > capacity || capacity > MAX_ROWS {
            return Err(LayoutError::Capacity);
        }
        let mut out = Self {
            ids: Vec::with_capacity(rows.len()),
            content: vec![0; rows.len() * ROW_BYTES],
            display: vec![0; rows.len() * RULE_BYTES],
            members: Vec::with_capacity(rows.len()),
            powers: Vec::with_capacity(rows.len()),
        };
        for (i, row) in rows.iter().enumerate() {
            if row.content.len() > ROW_BYTES || row.display.len() > RULE_BYTES {
                return Err(LayoutError::Text);
            }
            if row.content.last() == Some(&0) || row.display.last() == Some(&0) {
                return Err(LayoutError::EmbeddedZero);
            }
            out.ids.push(row.id);
            out.members.push(row.members);
            out.powers.push(row.power);
            out.content[i * ROW_BYTES..i * ROW_BYTES + row.content.len()]
                .copy_from_slice(&row.content);
            out.display[i * RULE_BYTES..i * RULE_BYTES + row.display.len()]
                .copy_from_slice(&row.display);
        }
        Ok(out)
    }
    pub fn unpack(&self) -> Vec<parallel_kernel::Event> {
        self.ids
            .iter()
            .enumerate()
            .map(|(i, id)| {
                let content = &self.content[i * ROW_BYTES..i * ROW_BYTES + ROW_BYTES];
                let display = &self.display[i * RULE_BYTES..i * RULE_BYTES + RULE_BYTES];
                let nc = content.iter().rposition(|b| *b != 0).map_or(0, |j| j + 1);
                let nd = display.iter().rposition(|b| *b != 0).map_or(0, |j| j + 1);
                parallel_kernel::Event {
                    id: *id,
                    content: content[..nc].to_vec(),
                    display: display[..nd].to_vec(),
                    members: self.members[i],
                    power: self.powers[i],
                }
            })
            .collect()
    }
}
