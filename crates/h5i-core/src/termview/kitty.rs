//! The Kitty graphics protocol, generated **by the viewer and only by the
//! viewer**.
//!
//! This module is where the boundary's central claim about the terminal is
//! actually cashed in. Terminal output is not text: an escape sequence can
//! rewrite the system clipboard (OSC 52), retitle the window, emit a hyperlink,
//! or ask this very protocol to read a file off disk. So no byte the box
//! produces is ever written to the terminal. The box supplies compressed pixels
//! inside a WebSocket message; the viewer decodes them, and *the viewer* writes
//! the escape sequences. There is no path from the box's output to the host's
//! PTY, which is the property a terminal viewer has to earn.
//!
//! Three choices in here follow from that, and each would be wrong in an
//! ordinary image viewer:
//!
//! * **`q=2` on every render command.** The terminal answers graphics commands
//!   with APC sequences *on stdin*. Those replies would land in the middle of
//!   the keystrokes we are translating into page input — at best noise, at
//!   worst a terminal's reply forwarded into the page as typing. `q=2`
//!   suppresses both the OK and the error responses, so the input stream stays
//!   exactly what the human typed. The cost is that render errors are silent,
//!   which is the right trade for the one direction that must stay clean.
//! * **Direct transmission (`t=d`) only.** The protocol can also read a file
//!   path, a temporary file, or a POSIX shared-memory object. Those are faster,
//!   and they are the fast path this module deliberately does not take yet:
//!   they only work when the terminal is on this machine, and working over SSH
//!   is half the point of a terminal viewer. Bytes are kept down by scaling the
//!   image to the cells it will occupy instead (see [`super::image`]).
//! * **Explicit deletion of the previous frame.** Every frame is a new image,
//!   and a terminal asked to hold thousands of them will hold them. The
//!   previous id is deleted *after* the new one is placed, so the viewport
//!   never blinks through an empty cell box.

/// Chunk size for the base64 payload. Fixed by the protocol: a single escape
/// sequence's payload may not exceed 4096 bytes.
const CHUNK: usize = 4096;

/// Image ids the viewer cycles through.
///
/// Two is all it takes: place the new frame, then delete the one before it. A
/// single id would mean deleting the image that is currently on screen before
/// its replacement exists, which is visible as a flicker on every frame.
const IDS: [u32; 2] = [7311, 7312];

/// Builds the escape sequences for successive frames.
#[derive(Debug, Default)]
pub struct Placer {
    /// Index into [`IDS`] for the *next* frame.
    next: usize,
    /// The id currently on screen, if any.
    live: Option<u32>,
}

/// Where and how large a frame should be drawn, in terminal cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    /// 1-based terminal row of the image's top edge.
    pub row: u16,
    /// 1-based terminal column of the image's left edge.
    pub col: u16,
    pub cols: u16,
    pub rows: u16,
}

impl Placer {
    pub fn new() -> Self {
        Placer::default()
    }

    /// The full byte sequence that draws one RGB frame at `at`.
    ///
    /// `rgb` is tightly packed 8-bit RGB, `width * height * 3` bytes. Returned
    /// as bytes rather than written directly so the whole frame reaches the
    /// terminal in one `write_all` — a partially written frame is a visible
    /// tear, and interleaving with the status line would be worse.
    pub fn draw(&mut self, rgb: &[u8], width: u32, height: u32, at: Placement) -> Vec<u8> {
        let id = IDS[self.next];
        self.next = (self.next + 1) % IDS.len();

        let mut out = Vec::with_capacity(rgb.len() * 4 / 3 + 512);
        // Park the cursor at the image's top-left. The placement itself carries
        // `C=1`, so the cursor stays here afterwards and the next frame lands in
        // the same spot without any further positioning.
        out.extend_from_slice(cursor_to(at.row, at.col).as_bytes());

        let payload = base64_of(rgb);
        for chunk in transmit_chunks(id, width, height, at.cols, at.rows, &payload) {
            out.extend_from_slice(chunk.as_bytes());
        }

        // Only now that the replacement is on screen.
        if let Some(old) = self.live.replace(id) {
            out.extend_from_slice(delete(old).as_bytes());
        }
        out
    }

    /// Remove whatever is on screen. Used on the way out, so a viewer that
    /// exits does not leave a frozen frame behind in the scrollback.
    pub fn clear(&mut self) -> Vec<u8> {
        match self.live.take() {
            Some(id) => delete(id).into_bytes(),
            None => Vec::new(),
        }
    }
}

fn base64_of(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// `\x1b[<row>;<col>H`
pub fn cursor_to(row: u16, col: u16) -> String {
    format!("\x1b[{row};{col}H")
}

/// Delete an image and free the terminal's copy of its data.
fn delete(id: u32) -> String {
    // Uppercase `I` frees the data as well as the placements; lowercase would
    // leave the terminal holding every frame we ever sent.
    format!("\x1b_Ga=d,d=I,i={id},q=2\x1b\\")
}

/// Split one frame into protocol-legal chunks.
///
/// The first chunk carries the control data; continuation chunks carry only
/// `m`, as the protocol requires. `m=1` means "more coming", `m=0` ends it.
pub fn transmit_chunks(
    id: u32,
    width: u32,
    height: u32,
    cols: u16,
    rows: u16,
    payload: &str,
) -> Vec<String> {
    let mut chunks: Vec<&str> = payload
        .as_bytes()
        .chunks(CHUNK)
        // Safe: base64 is ASCII, so a byte-boundary split is a char boundary.
        .map(|c| std::str::from_utf8(c).unwrap_or_default())
        .collect();
    if chunks.is_empty() {
        chunks.push("");
    }

    let last = chunks.len() - 1;
    chunks
        .into_iter()
        .enumerate()
        .map(|(i, chunk)| {
            let more = u8::from(i != last);
            if i == 0 {
                format!(
                    // a=T   transmit and display in one command
                    // f=24  8-bit RGB, which is what the JPEG decoder hands us
                    // t=d   the payload is right here, not a path we hand over
                    // s,v   source pixel dimensions
                    // c,r   the cell box to scale into
                    // C=1   leave the cursor alone, so the status line stays put
                    // q=2   no reply on stdin (see the module docs)
                    "\x1b_Ga=T,f=24,t=d,i={id},s={width},v={height},c={cols},r={rows},C=1,q=2,m={more};{chunk}\x1b\\"
                )
            } else {
                format!("\x1b_Gm={more};{chunk}\x1b\\")
            }
        })
        .collect()
}

// ─── capability detection ───────────────────────────────────────────────────

/// Ask the terminal whether it speaks the graphics protocol.
///
/// The trick in the second half is the load-bearing part. A terminal that does
/// not know the graphics protocol does not answer the query — it says nothing,
/// and there is no timeout that distinguishes "not supported" from "slow". So
/// the probe is followed by a Primary Device Attributes request, which every
/// terminal since the VT100 answers. Read until one of them arrives: a graphics
/// reply means yes, a device-attributes reply arriving *first* means the
/// graphics query was silently dropped, which means no.
///
/// `q=0` here, unlike every render command: this is the one place a reply is
/// what we are after.
pub fn probe_sequence() -> String {
    // A 1x1 RGB pixel, transmitted but not displayed (`a=q` is query-only).
    let pixel = base64_of(&[0u8, 0, 0]);
    format!("\x1b_Gi=1,s=1,v=1,a=q,t=d,f=24;{pixel}\x1b\\\x1b[c")
}

/// What a terminal said in response to [`probe_sequence`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Support {
    /// A graphics reply arrived: the protocol is understood.
    Yes,
    /// Device attributes arrived without a graphics reply: it is not.
    No,
    /// Neither has arrived yet; keep reading.
    Undecided,
}

/// Classify whatever has arrived so far.
///
/// Order matters and is the whole point: the graphics reply is checked first,
/// because a terminal that supports graphics answers *both* queries and the
/// device-attributes reply may well be in the same read.
pub fn classify_probe(seen: &[u8]) -> Support {
    if find(seen, b"\x1b_G").is_some() {
        return Support::Yes;
    }
    // CSI ? ... c — the device-attributes reply, and our "nothing else is
    // coming" marker.
    if let Some(start) = find(seen, b"\x1b[?") {
        if seen[start..].contains(&b'c') {
            return Support::No;
        }
    }
    Support::Undecided
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pull the control block out of `\x1b_G<control>;<payload>\x1b\\`.
    fn control_of(seq: &str) -> &str {
        seq.strip_prefix("\x1b_G")
            .unwrap()
            .split(';')
            .next()
            .unwrap()
    }

    #[test]
    fn every_render_command_silences_the_terminals_reply() {
        // The reply would arrive on stdin, in the middle of the keystrokes this
        // viewer is translating into page input. `q=2` is not a preference.
        let mut p = Placer::new();
        let frame = p.draw(&[0u8; 12], 2, 2, Placement { row: 2, col: 1, cols: 10, rows: 5 });
        let text = String::from_utf8(frame).unwrap();
        for seq in text.split_inclusive("\x1b\\") {
            if seq.contains("\x1b_G") && !seq.contains("m=1;") {
                assert!(seq.contains("q=2"), "a graphics command without q=2: {seq:?}");
            }
        }
    }

    #[test]
    fn the_first_chunk_carries_the_control_data_and_the_rest_carry_only_m() {
        // Fixed by the protocol: control keys on continuation chunks are an
        // error, and a chunk over 4096 bytes is rejected outright.
        let payload = "A".repeat(CHUNK * 2 + 10);
        let chunks = transmit_chunks(9, 4, 4, 20, 10, &payload);
        assert_eq!(chunks.len(), 3);

        let first = control_of(&chunks[0]);
        assert!(first.contains("a=T"), "{first}");
        assert!(first.contains("f=24"), "{first}");
        assert!(first.contains("t=d"), "{first}");
        assert!(first.contains("s=4,v=4"), "{first}");
        assert!(first.contains("c=20,r=10"), "{first}");
        assert!(first.contains("C=1"), "the cursor must not move: {first}");
        assert!(first.ends_with("m=1"), "{first}");

        assert_eq!(control_of(&chunks[1]), "m=1");
        assert_eq!(control_of(&chunks[2]), "m=0");

        for c in &chunks {
            let body = c.split_once(';').unwrap().1.trim_end_matches("\x1b\\");
            assert!(body.len() <= CHUNK, "chunk payload over the protocol limit");
        }
    }

    #[test]
    fn a_single_chunk_frame_ends_the_command_immediately() {
        let chunks = transmit_chunks(1, 1, 1, 1, 1, "AAAA");
        assert_eq!(chunks.len(), 1);
        assert!(control_of(&chunks[0]).ends_with("m=0"));
    }

    #[test]
    fn the_previous_frame_is_deleted_only_after_the_new_one_is_placed() {
        // Deleting first is a blink on every single frame.
        let mut p = Placer::new();
        let at = Placement { row: 2, col: 1, cols: 8, rows: 4 };

        let first = String::from_utf8(p.draw(&[0u8; 3], 1, 1, at)).unwrap();
        assert!(!first.contains("a=d"), "nothing to delete on the first frame");

        let second = String::from_utf8(p.draw(&[0u8; 3], 1, 1, at)).unwrap();
        let place_at = second.find("a=T").unwrap();
        let delete_at = second.find("a=d").expect("the old frame must be deleted");
        assert!(place_at < delete_at, "delete must follow the new placement");
        // And it must free the data, not just the placement, or the terminal
        // accumulates every frame of the session.
        assert!(second.contains("d=I"), "{second:?}");

        // The two frames must not share an id, or the delete would remove the
        // image that was just placed.
        let id_of = |s: &str| {
            let i = s.find("i=").unwrap();
            s[i..].split(',').next().unwrap().to_string()
        };
        assert_ne!(id_of(&first), id_of(&second));
    }

    #[test]
    fn the_frame_is_positioned_before_it_is_drawn() {
        let mut p = Placer::new();
        let seq = String::from_utf8(p.draw(&[0u8; 3], 1, 1, Placement { row: 3, col: 1, cols: 2, rows: 2 })).unwrap();
        // Row 3, because row 1 is the status line the viewer owns and row 2 is
        // the separator. The image must never start at row 1.
        assert!(seq.starts_with("\x1b[3;1H"), "{seq:?}");
    }

    #[test]
    fn leaving_removes_the_last_frame() {
        let mut p = Placer::new();
        assert!(p.clear().is_empty(), "nothing drawn, nothing to clear");
        p.draw(&[0u8; 3], 1, 1, Placement { row: 1, col: 1, cols: 1, rows: 1 });
        let out = String::from_utf8(p.clear()).unwrap();
        assert!(out.contains("a=d"), "{out:?}");
        assert!(p.clear().is_empty(), "clearing twice must not re-delete");
    }

    #[test]
    fn a_terminal_that_ignores_the_query_is_read_as_unsupported() {
        // The case the device-attributes fallback exists for: silence is
        // indistinguishable from slowness, so we ask a question every terminal
        // answers and use its reply as the barrier.
        assert_eq!(classify_probe(b""), Support::Undecided);
        assert_eq!(classify_probe(b"\x1b[?"), Support::Undecided, "reply still arriving");
        assert_eq!(classify_probe(b"\x1b[?62;1;6c"), Support::No);

        // A graphics reply means yes, whatever else arrived with it.
        assert_eq!(classify_probe(b"\x1b_Gi=1;OK\x1b\\"), Support::Yes);
        // And a terminal that supports graphics answers *both*, often in one
        // read. Checking device attributes first would call that a no.
        assert_eq!(
            classify_probe(b"\x1b_Gi=1;OK\x1b\\\x1b[?62;1;6c"),
            Support::Yes
        );
        // An error reply is still a reply: the protocol is understood.
        assert_eq!(classify_probe(b"\x1b_Gi=1;ENOTSUPPORTED\x1b\\"), Support::Yes);
    }

    #[test]
    fn the_probe_asks_for_a_reply_where_the_render_path_suppresses_one() {
        let p = probe_sequence();
        assert!(p.contains("a=q"), "query only, nothing is displayed: {p:?}");
        assert!(!p.contains("q=2"), "this is the one place we want an answer");
        assert!(p.ends_with("\x1b[c"), "the device-attributes barrier: {p:?}");
    }
}
