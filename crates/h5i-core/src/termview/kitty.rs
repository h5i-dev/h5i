//! The Kitty graphics protocol, generated **by the viewer and only by the
//! viewer**.
//!
//! Terminal output is not text: an escape sequence can rewrite the clipboard
//! (OSC 52), retitle the window, or ask this protocol to read a file off disk.
//! So no byte the box produces reaches the terminal. The box supplies compressed
//! pixels inside a WebSocket message, the viewer decodes them, and the viewer
//! writes the escapes. Four choices follow, each wrong in an ordinary image
//! viewer:
//!
//! * **`q=2` on every render command.** The terminal answers graphics commands
//!   with APC sequences *on stdin*, which would land among the keystrokes being
//!   translated into page input: noise at best, a terminal's reply forwarded
//!   into the page as typing at worst. The cost is silent render errors, which
//!   is the right trade for the direction that must stay clean.
//! * **Direct transmission (`t=d`) only.** File paths, temporary files and
//!   shared memory are faster and work only when the terminal is on this
//!   machine, and working over SSH is half the point. Bytes come down by scaling
//!   to the cells the image occupies ([`super::image`]) and deflating the rest.
//! * **Compression is probed, not assumed.** Every implementation is expected to
//!   have `o=z`, but `q=2` means one that does not would fail *silently*: a
//!   blank pane and no diagnosis. So the probe asks twice, raw and deflated, and
//!   frames compress only if the terminal said `OK` ([`accepts_zlib`]).
//! * **Explicit deletion of the previous frame**, after the new one is placed,
//!   or a terminal asked to hold thousands of images will hold them and the
//!   viewport blinks through an empty cell box.

/// Chunk size for the base64 payload. Fixed by the protocol: a single escape
/// sequence's payload may not exceed 4096 bytes.
const CHUNK: usize = 4096;

/// Image ids the viewer cycles through.
///
/// Two is all it takes: place the new frame, then delete the one before it. A
/// single id would mean deleting the image that is currently on screen before
/// its replacement exists, which is visible as a flicker on every frame.
const IDS: [u32; 2] = [7311, 7312];

/// zlib level for frame payloads.
///
/// Level 1, and the gap is not close. Measured through this path on a real
/// decoded frame scaled to 891x504, which is what a 120x30 pane asks for:
///
/// | level | ratio | cost   |
/// |-------|-------|--------|
/// | 1     | 6.2x  | 3.6 ms |
/// | 6     | 6.9x  | 21 ms  |
/// | 9     | 7.0x  | 63 ms  |
///
/// Level 1 takes essentially all of the win. The other two spend a frame budget
/// that also has to hold a JPEG decode and a box filter, to save a few percent
/// of a payload that is already an order of magnitude smaller than it was.
const ZLIB_LEVEL: u8 = 1;

/// How a frame's pixels are encoded before base64.
///
/// Not a tuning knob: it records what *this terminal* answered to the probe.
/// Raw is what a terminal always accepts and costs about six times the bytes,
/// so it is the answer only when nothing told us otherwise.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Encoding {
    /// Tightly packed 8-bit RGB, exactly as the decoder handed it over.
    #[default]
    Raw,
    /// The same bytes, RFC 1950 deflated. `o=z` in the control block.
    Zlib,
}

impl Encoding {
    /// The control key this encoding adds, if any.
    fn key(self) -> &'static str {
        match self {
            Encoding::Raw => "",
            Encoding::Zlib => ",o=z",
        }
    }

    /// Encode one frame's pixels for transmission.
    fn apply(self, rgb: &[u8]) -> std::borrow::Cow<'_, [u8]> {
        match self {
            Encoding::Raw => std::borrow::Cow::Borrowed(rgb),
            Encoding::Zlib => std::borrow::Cow::Owned(
                miniz_oxide::deflate::compress_to_vec_zlib(rgb, ZLIB_LEVEL),
            ),
        }
    }
}

/// Builds the escape sequences for successive frames.
#[derive(Debug, Default)]
pub struct Placer {
    /// Index into [`IDS`] for the *next* frame.
    next: usize,
    /// The id currently on screen, if any.
    live: Option<u32>,
    /// What this terminal accepts, from the probe.
    encoding: Encoding,
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
    /// A placer for a terminal that encodes frames this way.
    pub fn new(encoding: Encoding) -> Self {
        Placer {
            encoding,
            ..Placer::default()
        }
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

        let payload = base64_of(&self.encoding.apply(rgb));
        for chunk in transmit_chunks(id, width, height, at.cols, at.rows, &payload, self.encoding) {
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
    encoding: Encoding,
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
    let o = encoding.key();
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
                    // o=z   deflated, when the terminal said it accepts that
                    // s,v   source pixel dimensions — of the *decompressed*
                    //       pixels, which is how the terminal sizes its buffer
                    // c,r   the cell box to scale into
                    // C=1   leave the cursor alone, so the status line stays put
                    // q=2   no reply on stdin (see the module docs)
                    "\x1b_Ga=T,f=24,t=d{o},i={id},s={width},v={height},c={cols},r={rows},C=1,q=2,m={more};{chunk}\x1b\\"
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
/// It asks two questions rather than one. The second is the same pixel under
/// `o=z`, and its answer is what licenses compressing frames: the render path
/// suppresses replies, so a terminal that does not understand `o=z` would drop
/// every frame in silence. Answering the raw query and erroring the compressed
/// one is a perfectly good terminal — it just has to be sent more bytes.
///
/// `q=0` here, unlike every render command: this is the one place a reply is
/// what we are after.
pub fn probe_sequence() -> String {
    // A 1x1 RGB pixel, transmitted but not displayed (`a=q` is query-only).
    let raw = base64_of(&[0u8, 0, 0]);
    let deflated = base64_of(&Encoding::Zlib.apply(&[0u8, 0, 0]));
    format!(
        "\x1b_Gi=1,s=1,v=1,a=q,t=d,f=24;{raw}\x1b\\\
         \x1b_Gi=2,s=1,v=1,a=q,t=d,f=24,o=z;{deflated}\x1b\\\
         \x1b[c"
    )
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
    if probe_done(seen) {
        return Support::No;
    }
    Support::Undecided
}

/// Has the device-attributes reply arrived?
///
/// It is the probe's barrier: every terminal since the VT100 answers it, and it
/// is queued behind both graphics queries, so its arrival means every answer
/// this terminal intends to give has been given. The read loop stops on it
/// rather than on the first graphics reply, because stopping early would mean
/// deciding [`accepts_zlib`] before the second answer had been read.
pub fn probe_done(seen: &[u8]) -> bool {
    // CSI ? ... c
    match find(seen, b"\x1b[?") {
        Some(start) => seen[start..].contains(&b'c'),
        None => false,
    }
}

/// Did the terminal accept the deflated half of the probe?
///
/// Only an explicit `OK` against the compressed query's own id counts. Silence
/// is not consent here: this decides whether every subsequent frame is sent in
/// a form the terminal may not understand, and `q=2` means being wrong about it
/// is a blank pane with nothing on stdin to explain it.
pub fn accepts_zlib(seen: &[u8]) -> bool {
    replies(seen)
        .iter()
        .any(|(control, message)| control.split(',').any(|k| k == "i=2") && message == "OK")
}

/// Split whatever arrived into `(control, message)` for each APC reply.
fn replies(seen: &[u8]) -> Vec<(String, String)> {
    let text = String::from_utf8_lossy(seen);
    let mut rest: &str = &text;
    let mut out = Vec::new();
    while let Some(start) = rest.find("\x1b_G") {
        rest = &rest[start + 3..];
        let Some(end) = rest.find("\x1b\\") else { break };
        let (block, after) = (&rest[..end], &rest[end + 2..]);
        rest = after;
        let (control, message) = block.split_once(';').unwrap_or((block, ""));
        out.push((control.to_string(), message.to_string()));
    }
    out
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
        let mut p = Placer::new(Encoding::Zlib);
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
        let chunks = transmit_chunks(9, 4, 4, 20, 10, &payload, Encoding::Raw);
        assert_eq!(chunks.len(), 3);

        let first = control_of(&chunks[0]);
        assert!(first.contains("a=T"), "{first}");
        assert!(first.contains("f=24"), "{first}");
        assert!(first.contains("t=d"), "{first}");
        assert!(!first.contains("o=z"), "raw must not claim compression: {first}");
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
        let chunks = transmit_chunks(1, 1, 1, 1, 1, "AAAA", Encoding::Raw);
        assert_eq!(chunks.len(), 1);
        assert!(control_of(&chunks[0]).ends_with("m=0"));
    }

    /// Reassemble a drawn frame's payload: every chunk's base64, concatenated
    /// and decoded, which is exactly what the terminal does with it.
    fn payload_of(frame: &[u8]) -> Vec<u8> {
        use base64::Engine as _;
        let text = String::from_utf8(frame.to_vec()).unwrap();
        let mut b64 = String::new();
        for block in text.split("\x1b_G").skip(1) {
            let block = block.split("\x1b\\").next().unwrap_or_default();
            let Some((control, body)) = block.split_once(';') else { continue };
            // Transmission chunks only: the trailing delete carries no payload.
            if control.contains("a=d") {
                continue;
            }
            b64.push_str(body);
        }
        base64::engine::general_purpose::STANDARD.decode(&b64).unwrap()
    }

    #[test]
    fn a_compressed_frame_inflates_back_to_exactly_the_pixels_it_was_given() {
        // The assertion that matters. Everything else about compression is a
        // control key being present; this is whether the terminal, doing what
        // the protocol says, gets the frame back. `q=2` means being wrong here
        // is a blank pane with nothing on stdin to explain it.
        let (w, h) = (64u32, 48u32);
        let rgb: Vec<u8> = (0..(w * h * 3) as usize)
            .map(|i| ((i * 7 + i / 191) % 256) as u8)
            .collect();
        let at = Placement { row: 3, col: 1, cols: 8, rows: 4 };

        let mut z = Placer::new(Encoding::Zlib);
        let compressed = z.draw(&rgb, w, h, at);
        let inflated =
            miniz_oxide::inflate::decompress_to_vec_zlib(&payload_of(&compressed)).unwrap();
        assert_eq!(inflated, rgb, "the frame must survive the round trip");

        // Raw is unchanged and still the byte-for-byte pixels, so the two
        // encodings are the same frame said two ways.
        let mut r = Placer::new(Encoding::Raw);
        let raw = r.draw(&rgb, w, h, at);
        assert_eq!(payload_of(&raw), rgb);

        // `s` and `v` describe the *decompressed* image. A terminal sizes its
        // buffer from them and then inflates into it, so quietly reporting the
        // payload's length instead would be a frame that decodes into nothing.
        let control = String::from_utf8(compressed.clone())
            .unwrap()
            .split_once("\x1b_G")
            .unwrap()
            .1
            .split(';')
            .next()
            .unwrap()
            .to_string();
        assert!(control.contains("o=z"), "{control}");
        assert!(control.contains("s=64,v=48"), "{control}");

        // And it is the smaller of the two, which is the entire reason the key
        // is there.
        assert!(
            compressed.len() < raw.len(),
            "compressed {} vs raw {}",
            compressed.len(),
            raw.len()
        );
    }

    #[test]
    fn compression_is_used_only_when_the_terminal_said_ok_to_it() {
        // The probe asks twice. Silence about the second question, or an error
        // against it, means raw frames — a terminal that draws is not a
        // terminal that inflates, and `q=2` would make the difference invisible.
        assert!(accepts_zlib(b"\x1b_Gi=1;OK\x1b\\\x1b_Gi=2;OK\x1b\\"));
        assert!(!accepts_zlib(b"\x1b_Gi=1;OK\x1b\\\x1b_Gi=2;EINVAL:o\x1b\\"));
        assert!(!accepts_zlib(b"\x1b_Gi=1;OK\x1b\\"), "no answer is not a yes");
        assert!(!accepts_zlib(b""));
        // The id must match exactly: `i=2` answering is not `i=21` answering,
        // and a substring test would confuse them.
        assert!(!accepts_zlib(b"\x1b_Gi=21;OK\x1b\\"));
        // A reply carrying more keys than the id is still that reply.
        assert!(accepts_zlib(b"\x1b_GI=7,i=2;OK\x1b\\"));
    }

    #[test]
    fn the_probe_reads_on_until_the_device_attributes_reply() {
        // Stopping at the first graphics reply would decide compression before
        // the compressed query had been answered — always "no", every time.
        assert!(!probe_done(b"\x1b_Gi=1;OK\x1b\\"));
        assert!(!probe_done(b"\x1b_Gi=1;OK\x1b\\\x1b[?"), "reply still arriving");
        assert!(probe_done(b"\x1b_Gi=1;OK\x1b\\\x1b[?62;1;6c"));
    }

    #[test]
    fn the_previous_frame_is_deleted_only_after_the_new_one_is_placed() {
        // Deleting first is a blink on every single frame.
        let mut p = Placer::new(Encoding::Raw);
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
        let mut p = Placer::new(Encoding::Raw);
        let seq = String::from_utf8(p.draw(&[0u8; 3], 1, 1, Placement { row: 3, col: 1, cols: 2, rows: 2 })).unwrap();
        // Row 3, because row 1 is the status line the viewer owns and row 2 is
        // the separator. The image must never start at row 1.
        assert!(seq.starts_with("\x1b[3;1H"), "{seq:?}");
    }

    #[test]
    fn leaving_removes_the_last_frame() {
        let mut p = Placer::new(Encoding::Raw);
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

        // Two questions, and the second one has to actually be compressed —
        // asking about `o=z` with a raw payload would be answered `OK` by a
        // terminal that cannot inflate anything.
        assert_eq!(p.matches("a=q").count(), 2, "raw and deflated: {p:?}");
        let deflated = p.split("i=2,").nth(1).unwrap();
        assert!(deflated.contains("o=z"), "{deflated:?}");
        let body = deflated.split_once(';').unwrap().1.split("\x1b").next().unwrap();
        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::STANDARD.decode(body).unwrap();
        assert_eq!(
            miniz_oxide::inflate::decompress_to_vec_zlib(&bytes).unwrap(),
            vec![0u8, 0, 0],
            "the compressed query must carry a real deflate stream"
        );
    }
}
