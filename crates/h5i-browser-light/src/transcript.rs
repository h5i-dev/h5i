//! What a page's media *says*, when the page has written it down.
//!
//! A `<video>` is a hole in every reading this engine produces, so a page whose
//! substance is a forty-minute talk reads as a title and a play button. Most of
//! the time the content is already there in text: HTML has carried
//! `<track kind="captions">` since 2010, and a caption file is prose with
//! timestamps, which is the shape a model reads well. So this verb decodes
//! nothing. It finds the tracks the page declared, fetches them through the
//! broker like any other subresource, and parses the cues out.
//!
//! **This is not audio support.** [`crate::Capabilities::video`] stays `false`:
//! no decoder, no media element that plays, no `MediaSource`, and ROADMAP's "no
//! GStreamer, no PulseAudio" is not reopened. A text fetch over a URL the
//! document named moves no capability, so this needs no `--script` and no grant.
//!
//! What changes is that "this page has media" stops being the end of the answer.
//! A media element with no track is reported as one, with its source URL,
//! because that routes a caller somewhere else while silence reads as "no media"
//! and is simply wrong.
//!
//! The fence applies: a caption file is a stranger's bytes landing in front of a
//! model, so it is [`collapse`]d per cue. Arriving as a subtitle rather than a
//! heading buys it no more trust.

use blitz_dom::{BaseDocument, Node};
use serde::{Deserialize, Serialize};

use crate::snapshot::collapse;

/// How many media elements to report on one page.
///
/// A page with fifty embedded players is a page an agent should be told about
/// rather than shown all of. Bounded, and the truncation is named.
const MAX_MEDIA: usize = 32;

/// How many tracks to list per media element.
///
/// Listing is cheap; *fetching* is what [`Selection`] bounds. A well-localised
/// player declares thirty languages and that list is worth seeing whole.
const MAX_TRACKS: usize = 64;

/// How many cues to carry out of one track.
///
/// A two-hour talk captioned at two seconds a cue is about 3,600, so this
/// admits a long one and still refuses a file that is trying to fill a context
/// window.
const MAX_CUES: usize = 4096;

/// The longest single cue worth carrying.
const MAX_CUE_BYTES: usize = 1024;

/// How much caption text one track may contribute, before truncation.
///
/// The cap a caller actually feels, and the one a `--max-bytes` overrides. Cue
/// count alone does not bound size: nothing stops a file from putting a
/// kilobyte in each of four thousand cues.
pub const DEFAULT_MAX_BYTES: usize = 256 * 1024;

/// A media element on the page, and what it carries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Media {
    /// `video` or `audio`, as the page spelled it.
    pub kind: String,
    /// A CSS selector that names this element, when the page gave it one.
    ///
    /// `#player` from an `id`, and nothing otherwise. Deliberately not
    /// synthesised from a position: `video:nth-of-type(2)` is scoped to a
    /// parent rather than to the document, so on a page whose players sit in
    /// different containers it names the wrong element — and a handle that
    /// resolves to the wrong thing is worse than no handle, because a caller
    /// acts on it. A `@ref` is not minted here either: refs are checked against
    /// the reading that minted them, and this is not a snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
    /// Where the media itself lives, resolved absolute.
    ///
    /// From `src`, or from the first `<source>` when the element uses those.
    /// Reported and never fetched: this verb does not move media bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub src: Option<String>,
    /// What the page calls it: `title`, or `aria-label`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Every `<track>` the element declares, in document order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tracks: Vec<Track>,
}

impl Media {
    /// True when the page ships this media with no timed text at all.
    ///
    /// The fact that routes a caller to another lane, so it is answered rather
    /// than left to be inferred from an empty list.
    pub fn is_silent(&self) -> bool {
        !self.tracks.iter().any(|t| t.carries_text())
    }
}

/// One `<track>`, before or after it has been read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Track {
    /// `subtitles`, `captions`, `descriptions`, `chapters` or `metadata`.
    ///
    /// Normalised the way the HTML parser does it: a missing or unrecognised
    /// `kind` is `subtitles`, which is the standard's own default and not a
    /// guess made here.
    pub kind: String,
    /// `srclang`, as written. BCP 47, unvalidated: a page that writes `EN` is
    /// reported as having written `EN`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// The human label the page put in the track menu.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Whether the page marked this one `default`.
    pub default: bool,
    /// Resolved absolute, because a relative one means nothing to a caller
    /// that is not going to resolve it against the same base.
    pub src: String,
    /// Whether this track was fetched, or only listed.
    ///
    /// A listed-not-fetched track is the normal case: [`chosen`] reads at most
    /// two per media element, one of the words and one outline, so every other
    /// language a page declares is listed and not read. An agent reading the
    /// reply needs the difference between "no cues" and "not asked for".
    pub fetched: bool,
    /// The receipt this fetch was recorded under.
    ///
    /// Carried so a caller can point at the row in `h5i browser requests` that
    /// paid for this text. A transcript with no receipt beside it is exactly
    /// the shape this engine exists to refuse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    /// Why the fetch or the parse produced nothing, when it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The cues, in file order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cues: Vec<Cue>,
    /// Named when the cues were cut short, so a partial transcript is visibly
    /// partial rather than quietly wrong. A model handed the first ten minutes
    /// of a talk with no note would summarise it as the whole talk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncated: Option<String>,
}

impl Track {
    /// Whether this kind of track holds words a reader wants.
    ///
    /// `metadata` is machine payload the page reads with script, and
    /// `descriptions` is for a screen reader rather than a transcript.
    /// `chapters` stays in: chapter titles are an outline of the media, which
    /// is the cheapest useful reading of it there is.
    pub fn carries_text(&self) -> bool {
        SPOKEN.contains(&self.kind.as_str()) || OUTLINE.contains(&self.kind.as_str())
    }

    /// The cues as `[MM:SS] text`, which is the shape a model reads.
    ///
    /// Timestamps are kept rather than stripped, because half of what an agent
    /// does with a transcript is point back into the media: "at 12:40 they say"
    /// is a citation, and prose with the clock removed cannot make one.
    pub fn text(&self) -> String {
        self.cues
            .iter()
            .map(|cue| format!("[{}] {}", stamp(cue.start), cue.text))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Track kinds that carry what was **said**.
///
/// One list, referenced by both the predicate and the selection. They were
/// written out separately for a while, which is two places that have to agree
/// about the same fact and can therefore stop agreeing.
const SPOKEN: &[&str] = &["subtitles", "captions"];

/// Track kinds that carry an **outline** of what was said.
///
/// Apart from [`SPOKEN`] because the two are chosen independently: a page gets
/// one of each, since thirty languages are the same words thirty times while an
/// outline is different information.
const OUTLINE: &[&str] = &["chapters"];

/// One timed line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cue {
    /// Seconds from the start of the media.
    pub start: f64,
    pub end: f64,
    /// Collapsed, so it cannot span a line. See the module note on the fence.
    pub text: String,
}

/// Which tracks to actually fetch.
///
/// At most two per media element: what was **said**, and the **outline** of it.
///
/// Not all of them, because a well-localised player declares thirty languages
/// and fetching every one is thirty requests to answer a question about one.
/// The listing is complete either way, so a caller that wanted a different
/// language can see it is there and ask again.
///
/// There used to be an `all` here that fetched every readable track, and it was
/// the wrong axis. Thirty languages of one video are the same words thirty
/// times; a `chapters` track is *different information*, an outline rather than
/// a translation. Sorting them into one flag meant the only thing that flag was
/// genuinely good for could not be had without also paying for the twenty-nine
/// that were redundant. Chapters are read by default now, and the flag is gone.
#[derive(Debug, Clone, PartialEq)]
pub struct Selection {
    /// Prefer this language. Matched against `srclang` case-insensitively, by
    /// prefix, so `en` matches `en-GB`.
    pub language: Option<String>,
    /// The per-track ceiling on caption text.
    pub max_bytes: usize,
}

impl Default for Selection {
    fn default() -> Self {
        Self {
            language: None,
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }
}

/// Everything this page carries, read.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Transcript {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub media: Vec<Media>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

impl Transcript {
    /// True when the page carries no media element at all.
    ///
    /// Distinct from "media with no captions", which is [`Media::is_silent`],
    /// and the two want different advice: one says look elsewhere on the page,
    /// the other says this media has no text lane.
    pub fn is_empty(&self) -> bool {
        self.media.is_empty()
    }

    /// Media that is present and carries nothing readable.
    pub fn silent(&self) -> Vec<&Media> {
        self.media.iter().filter(|m| m.is_silent()).collect()
    }

    /// Tracks that were fetched and did not yield cues, with the reason.
    ///
    /// The difference between "this page has no text lane" and "this page has
    /// one and it did not load" — a policy refusal, a 5xx, a body that is not
    /// WebVTT. Both end with no cues, and reporting the second as the first
    /// tells an agent to route away from a page whose captions are right there
    /// and were simply not delivered.
    pub fn read_failures(&self) -> Vec<(&Media, &Track)> {
        self.media
            .iter()
            .flat_map(|media| {
                media
                    .tracks
                    .iter()
                    // `error.is_some()` alone. A track whose `src` will not
                    // parse as a URL gets an error and never reaches
                    // `fetched = true`, so requiring both let exactly that case
                    // fall through to "its words exist only in the audio" —
                    // the misreport this method was added to prevent. A track
                    // that was listed and not asked for has no error, so it is
                    // still not a failure.
                    .filter(|track| track.error.is_some())
                    .map(move |track| (media, track))
            })
            .collect()
    }

    /// How many cues were actually read, across everything.
    pub fn cue_count(&self) -> usize {
        self.media
            .iter()
            .flat_map(|m| m.tracks.iter())
            .map(|t| t.cues.len())
            .sum()
    }

    /// The reading, fenced, as a model should receive it.
    ///
    /// Fenced over the finished document rather than per value, the way
    /// [`crate::markdown`] does it: the per-line invariant holds for a cue,
    /// which is collapsed, but the assembled transcript spans lines and the
    /// invariant does not survive the assembly.
    ///
    /// A track that was listed and not fetched says so, and a media element
    /// with no text lane says *that*, because both are answers. A reader handed
    /// a page with one captioned video and one silent one must be able to tell
    /// which is which, or it will report the whole page as transcribed.
    pub fn render(&self, url: &str) -> String {
        let mut out = format!("url: {url}\n");
        if self.media.is_empty() {
            out.push_str("media: none on this page\n");
        } else {
            out.push_str(&format!(
                "media: {} element(s), {} with timed text, {} cue(s) read\n",
                self.media.len(),
                self.media.iter().filter(|m| !m.is_silent()).count(),
                self.cue_count(),
            ));
        }
        for note in &self.notes {
            out.push_str(&format!("note: {note}\n"));
        }

        let mut body = String::new();
        for media in &self.media {
            body.push_str(&format!("\n## {}", media.kind));
            if let Some(label) = &media.label {
                body.push_str(&format!(" — {label}"));
            }
            body.push('\n');
            if let Some(src) = &media.src {
                body.push_str(&format!("source: {src}\n"));
            }
            if media.tracks.is_empty() {
                body.push_str(
                    "no timed text: this media declares no `<track>` at all, so there is \
                     nothing on the page to read. Its words exist only in the audio.\n",
                );
                continue;
            }
            for track in &media.tracks {
                body.push_str(&format!("\ntrack: {}", track.kind));
                if let Some(lang) = &track.language {
                    body.push_str(&format!(" [{lang}]"));
                }
                if let Some(label) = &track.label {
                    body.push_str(&format!(" \"{label}\""));
                }
                if track.default {
                    body.push_str(" (default)");
                }
                if let Some(seq) = track.seq {
                    body.push_str(&format!(" — receipt #{seq}"));
                }
                body.push('\n');

                if let Some(error) = &track.error {
                    body.push_str(&format!("not read: {error}\n"));
                    continue;
                }
                if !track.fetched {
                    // `--lang` is only a lever for the kinds this verb reads. A
                    // `descriptions` or `metadata` track is never selected
                    // whatever language is named, so offering that flag there is
                    // a dead end: the caller reruns, gets the same line back,
                    // and has learned nothing. Say what is actually true of it.
                    body.push_str(if track.carries_text() {
                        "listed, not read. Name its language with `--lang`.\n"
                    } else {
                        "not read: this kind of track is not a transcript. \
                         `metadata` is payload the page reads with script, and \
                         `descriptions` is written for a screen reader.\n"
                    });
                    continue;
                }
                if let Some(note) = &track.truncated {
                    body.push_str(&format!("note: {note}\n"));
                }
                body.push('\n');
                body.push_str(&track.text());
                body.push('\n');
            }
        }

        out.push_str(crate::snapshot::CONTENT_BEGIN);
        out.push('\n');
        out.push_str(crate::snapshot::UNTRUSTED_NOTE);
        out.push('\n');
        out.push_str(&crate::snapshot::defang_fence(&body));
        if !body.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(crate::snapshot::CONTENT_END);
        out.push('\n');
        out
    }
}

/// Find the media a document declares, without touching the network.
///
/// Split from the fetch deliberately: discovery is a pure function of the DOM
/// and is the half worth testing exhaustively, and a caller that only wants to
/// know whether a page has captions at all should not have to spend a request
/// to find out.
pub fn discover(doc: &BaseDocument, base: &url::Url) -> Transcript {
    let mut out = Transcript::default();
    let mut truncated_tracks = false;
    // The raw `id` of each media element, in the same order, resolved into
    // selectors after the walk. It cannot be decided during the walk: whether
    // `#player` names *this* element depends on whether anything later on the
    // page carries the same id.
    let mut ids: Vec<Option<String>> = Vec::new();

    for (_node_id, node) in doc.tree().iter() {
        let Some(element) = node.element_data() else {
            continue;
        };
        let tag = element.name.local.as_ref();
        if tag != "video" && tag != "audio" {
            continue;
        }
        if out.media.len() >= MAX_MEDIA {
            out.notes.push(format!(
                "this page has more than {MAX_MEDIA} media elements; the rest are not listed"
            ));
            break;
        }

        let mut media = Media {
            kind: tag.to_string(),
            // Filled in after the walk: whether `#player` names *this* element
            // depends on what the rest of the page carries.
            selector: None,
            src: attr(node, "src").and_then(|raw| resolve(base, &raw)),
            label: attr(node, "title")
                .or_else(|| attr(node, "aria-label"))
                .map(|raw| cap(collapse(&raw), MAX_CUE_BYTES))
                .filter(|text| !text.is_empty()),
            tracks: Vec::new(),
        };

        // `<source>` is how a page offers the same media in several codecs, and
        // an element that uses them has no `src` of its own. The first is
        // reported rather than all of them: they are the same content, and the
        // question this answers is "what is the media", not "in how many
        // containers".
        if media.src.is_none() {
            media.src = node
                .children
                .iter()
                .filter_map(|id| doc.get_node(*id))
                .find(|child| {
                    child
                        .element_data()
                        .is_some_and(|e| e.name.local.as_ref() == "source")
                })
                .and_then(|child| attr(child, "src"))
                .and_then(|raw| resolve(base, &raw));
        }

        for child_id in node.children.iter() {
            let Some(child) = doc.get_node(*child_id) else {
                continue;
            };
            let is_track = child
                .element_data()
                .is_some_and(|e| e.name.local.as_ref() == "track");
            if !is_track {
                continue;
            }
            // A `<track>` with no `src` declares nothing there is any way to
            // read, so it is skipped rather than listed as a track that always
            // fails.
            let Some(src) = attr(child, "src").and_then(|raw| resolve(base, &raw)) else {
                continue;
            };
            if media.tracks.len() >= MAX_TRACKS {
                truncated_tracks = true;
                break;
            }
            media.tracks.push(Track {
                kind: normalise_kind(attr(child, "kind").as_deref()),
                language: attr(child, "srclang")
                    .map(|raw| collapse(&raw))
                    .filter(|text| !text.is_empty()),
                label: attr(child, "label")
                    .map(|raw| cap(collapse(&raw), MAX_CUE_BYTES))
                    .filter(|text| !text.is_empty()),
                // Presence is the whole test. `default=""` and `default="false"`
                // both mean the attribute is there, which in HTML means true —
                // reading the *value* here is the classic way to get it
                // backwards.
                default: attr(child, "default").is_some(),
                src,
                fetched: false,
                seq: None,
                error: None,
                cues: Vec::new(),
                truncated: None,
            });
        }

        ids.push(attr(node, "id").map(|id| collapse(&id)).filter(|id| !id.is_empty()));
        out.media.push(media);
    }

    // Ids, resolved against the whole document rather than per element.
    //
    // `#dup` names the *first* element with that id, and duplicate ids are
    // legal in the wild — two copies of an embed snippet is the ordinary way it
    // happens. Handing the second `<video id="player">` back as `#player` gives
    // a caller a handle that acts on the first, and this field's own doc says a
    // handle that resolves to the wrong thing is worse than none. An id that is
    // not a legal CSS identifier (`video.main`) is dropped for the same reason:
    // `#video.main` parses as an id plus a class and matches something else.
    // `crate::selector` guards its ids both ways already.
    let mut seen: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for (_, node) in doc.tree().iter() {
        if let Some(id) = attr(node, "id").map(|id| collapse(&id)).filter(|id| !id.is_empty()) {
            *seen.entry(id).or_default() += 1;
        }
    }
    for (media, id) in out.media.iter_mut().zip(ids) {
        media.selector = id
            .filter(|id| is_css_ident(id))
            .filter(|id| seen.get(id).copied() == Some(1))
            .map(|id| format!("#{id}"));
    }

    if truncated_tracks {
        out.notes.push(format!(
            "a media element declares more than {MAX_TRACKS} tracks; the rest are not listed"
        ));
    }
    out
}

/// The HTML default for a `<track>` without a usable `kind`.
///
/// The standard's own enumerated-attribute rule: the missing value default and
/// the invalid value default are both `subtitles`. Written out because the
/// alternative — carrying `""` through and special-casing it downstream — is
/// how a track ends up excluded from a transcript for having omitted an
/// attribute it was allowed to omit.
fn normalise_kind(raw: Option<&str>) -> String {
    let value = raw.unwrap_or_default().trim().to_ascii_lowercase();
    match value.as_str() {
        "captions" | "descriptions" | "chapters" | "metadata" => value,
        _ => "subtitles".to_string(),
    }
}

/// Which tracks [`read`] would fetch, given a selection.
///
/// Indices into each media element's `tracks`, so a caller can report what it
/// is about to spend before spending it.
pub fn chosen(media: &Media, selection: &Selection) -> Vec<usize> {
    // The two kinds are picked separately because they answer different
    // questions. One track of the words, and one outline of them.
    let of_kind = |wanted: &[&str]| -> Vec<usize> {
        media
            .tracks
            .iter()
            .enumerate()
            .filter(|(_, track)| wanted.contains(&track.kind.as_str()))
            .map(|(index, _)| index)
            .collect()
    };

    // A language the caller named wins over the page's own default, because
    // the caller naming one is the more specific instruction. Prefix-matched:
    // `en` should find `en-GB`, and a page that ships only `en-GB` is not a
    // page with no English. Then the page's own `default`, then the first: a
    // page that marked a track default marked it for a reason, and guessing
    // past that would be the engine overruling the page about its own content.
    let pick = |among: Vec<usize>| -> Option<usize> {
        if let Some(want) = selection.language.as_deref().map(str::to_ascii_lowercase)
            && let Some(index) = among.iter().copied().find(|index| {
                media.tracks[*index]
                    .language
                    .as_deref()
                    .map(str::to_ascii_lowercase)
                    .is_some_and(|have| have.starts_with(&want) || want.starts_with(&have))
            })
        {
            return Some(index);
        }
        among
            .iter()
            .copied()
            .find(|index| media.tracks[*index].default)
            .or_else(|| among.first().copied())
    };

    let mut picked: Vec<usize> = pick(of_kind(SPOKEN))
        .into_iter()
        .chain(pick(of_kind(OUTLINE)))
        .collect();
    // Document order, so a reading of one page is the same reading twice.
    picked.sort_unstable();
    picked
}

/// Fetch the chosen tracks and parse them, through the broker like anything
/// else.
///
/// `document` is the page the tracks were declared on, and it is load-bearing
/// rather than bookkeeping: without it the policy reads a caption fetch as the
/// agent naming a URL, and `<track src="http://127.0.0.1:3000/…">` on a page
/// from the open web would reach the box's dev server. That is exactly the hole
/// [`crate::policy::Policy::check_from`] exists to close, and a new fetch path
/// that forgot to pass an origin would reopen it quietly.
pub fn read(
    transcript: &mut Transcript,
    broker: &dyn crate::broker::Broker,
    document: Option<&url::Url>,
    selection: &Selection,
) {
    for media in transcript.media.iter_mut() {
        for index in chosen(media, selection) {
            let url = match url::Url::parse(&media.tracks[index].src) {
                Ok(url) => url,
                Err(err) => {
                    media.tracks[index].error = Some(format!("unreadable track URL: {err}"));
                    continue;
                }
            };

            // A text track is a **CORS request** in a browser, and for exactly
            // the reason it matters here: the track's *text* is read and handed
            // to the agent as the transcript, so a cross-origin one is a
            // cross-origin read of somebody else's document. Without this a
            // page could point a `<track>` at any allowed origin and have the
            // engine fetch it, decode it and put it in front of the model.
            //
            // `document` is `None` when the agent named the media itself, which
            // is the agent exercising its own authority over a URL it chose —
            // the same distinction `crate::cors::Requester` draws.
            let outcome = match document {
                Some(document) => broker.send_script(
                    &url,
                    "GET",
                    &[],
                    None,
                    document,
                    &[],
                    crate::cors::Mode::Cors,
                    crate::cors::Credentials::SameOrigin,
                ),
                None => {
                    broker.fetch_from(&url, crate::receipt::Initiator::Subresource, None)
                }
            };
            let track = &mut media.tracks[index];
            track.fetched = true;
            track.seq = outcome.seq;

            if let Some(error) = outcome.error {
                track.error = Some(error);
                continue;
            }
            if outcome.status.is_some_and(|code| !(200..300).contains(&code)) {
                track.error = Some(format!(
                    "the server answered {} for this track",
                    outcome.status.unwrap_or_default()
                ));
                continue;
            }

            // WebVTT is UTF-8 by specification — not "usually", *by
            // specification* — so a lossy decode here is decoding a file that
            // is already out of spec, and replacement characters in one cue
            // are a better answer than discarding a whole transcript.
            let body = String::from_utf8_lossy(&outcome.body);
            let (cues, truncated) = parse(&body, selection.max_bytes);
            if cues.is_empty() && truncated.is_none() {
                track.error = Some(
                    "this track parsed to no cues. It may not be WebVTT, or it may be empty."
                        .to_string(),
                );
                continue;
            }
            track.cues = cues;
            track.truncated = truncated;
        }
    }
}

/// Parse WebVTT, and SRT, with one reader.
///
/// One parser rather than two because the difference between the formats is
/// almost nothing that matters here: SRT numbers its cues and separates its
/// milliseconds with a comma, WebVTT has a header line and cue settings after
/// the timings. Both are blank-line-separated blocks whose timing line holds
/// `-->`, so keying on that line and ignoring everything around it reads both —
/// and reads the badly-formed files in the wild that are neither.
///
/// Returns the cues and, when the text was cut short, a note saying so.
pub fn parse(body: &str, max_bytes: usize) -> (Vec<Cue>, Option<String>) {
    let mut cues: Vec<Cue> = Vec::new();
    let mut bytes = 0usize;
    let mut truncated: Option<String> = None;

    // `\r\n` and a UTF-8 BOM are both routine in files served as captions, and
    // both would otherwise turn the first timing line into an unparseable one.
    let body = body.trim_start_matches('\u{feff}').replace("\r\n", "\n");

    for block in body.split("\n\n") {
        let block = block.trim_matches('\n');
        if block.is_empty() {
            continue;
        }
        // WebVTT's own non-cue blocks. `NOTE` is a comment, `STYLE` is CSS and
        // `REGION` is layout; none holds words a reader wants, and STYLE in
        // particular would otherwise arrive as a cue full of selectors.
        let head = block.split('\n').next().unwrap_or_default().trim();
        if head == "WEBVTT" || head.starts_with("WEBVTT ") {
            continue;
        }
        if matches!(head, "NOTE" | "STYLE" | "REGION")
            || head.starts_with("NOTE ")
            || head.starts_with("STYLE ")
            || head.starts_with("REGION ")
        {
            continue;
        }

        let Some(timing_at) = block.split('\n').position(|line| line.contains("-->")) else {
            continue;
        };
        let lines: Vec<&str> = block.split('\n').collect();
        let Some((start, end)) = timings(lines[timing_at]) else {
            continue;
        };

        let text = lines[timing_at + 1..].join(" ");
        let text = cap(collapse(&strip_markup(&text)), MAX_CUE_BYTES);
        if text.is_empty() {
            continue;
        }

        if cues.len() >= MAX_CUES {
            truncated = Some(format!(
                "this track has more than {MAX_CUES} cues; the transcript stops there"
            ));
            break;
        }
        bytes += text.len();
        if bytes > max_bytes {
            truncated = Some(format!(
                "this transcript passed {max_bytes} bytes and stops there. \
                 Pass a larger `--max-bytes` for the rest."
            ));
            break;
        }
        cues.push(Cue { start, end, text });
    }

    (cues, truncated)
}

/// The two ends of a cue, from the line that holds `-->`.
///
/// Cue settings (`line:0 position:20% align:start`) follow the end timestamp on
/// the same line and are dropped: they say where the text is drawn, and nothing
/// here draws anything.
fn timings(line: &str) -> Option<(f64, f64)> {
    let (left, right) = line.split_once("-->")?;
    let start = seconds(left.trim())?;
    let end = seconds(right.split_whitespace().next()?)?;
    Some((start, end))
}

/// `HH:MM:SS.mmm`, `MM:SS.mmm`, and both with SRT's comma.
///
/// The hours field is optional in WebVTT and mandatory in SRT, so the parts are
/// counted from the right rather than the left: seconds is always last, minutes
/// always before it, and hours is there or it is not.
fn seconds(stamp: &str) -> Option<f64> {
    let stamp = stamp.trim().replace(',', ".");
    let parts: Vec<&str> = stamp.split(':').collect();
    let (hours, minutes, secs) = match parts.as_slice() {
        [h, m, s] => (h.parse::<f64>().ok()?, m.parse::<f64>().ok()?, *s),
        [m, s] => (0.0, m.parse::<f64>().ok()?, *s),
        _ => return None,
    };
    let secs: f64 = secs.parse().ok()?;
    // A negative component would order the transcript wrongly rather than
    // failing loudly, so it is refused here where it is still one cue.
    if hours < 0.0 || minutes < 0.0 || secs < 0.0 {
        return None;
    }
    Some(hours * 3600.0 + minutes * 60.0 + secs)
}

/// `[MM:SS]`, or `[HH:MM:SS]` once there are hours to show.
///
/// Fixed width within each form, because these are read down a column and a
/// ragged left edge makes a transcript harder to scan than the timestamps are
/// worth.
pub fn stamp(seconds: f64) -> String {
    let total = seconds.max(0.0) as u64;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h:02}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

/// Take the markup out of a cue.
///
/// WebVTT cue text carries a small tag vocabulary — `<v Speaker>`, `<i>`, `<c>`,
/// `<00:00:12.000>` for karaoke timing — and the five named entities. A model
/// handed `<v Roger>&amp;` reads markup; a model handed `Roger &` reads what was
/// said. The speaker name inside `<v …>` is kept, because who is speaking is
/// content.
fn strip_markup(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '<' => {
                // One pass, and the buffer is what makes the unterminated case
                // safe: scan to `>`, and if the input ends first put back the
                // `<` and everything after it rather than dropping it.
                //
                // Two cleverer versions preceded this and both were worse. A
                // 128-character bound made a legitimately long closed tag come
                // out as markup in the transcript. Looking ahead for `>` fixed
                // that and made this quadratic — a fresh copy of the remaining
                // text per `<`, on input that is *not yet* bounded, since
                // `MAX_CUE_BYTES` is applied to what this returns. A caption
                // body with no blank line and fifty thousand `<` is then tens
                // of gigabytes of copying: a hang, reachable from a file this
                // lane fetches and explicitly does not trust.
                let mut tag = String::new();
                let mut closed = false;
                for inner in chars.by_ref() {
                    if inner == '>' {
                        closed = true;
                        break;
                    }
                    tag.push(inner);
                }
                if !closed {
                    out.push('<');
                    out.push_str(&tag);
                    continue;
                }
                // `<v Roger Bannister>` names the speaker of everything after
                // it. Rendered as a prefix rather than dropped: a two-person
                // interview with the attributions removed is a transcript
                // nobody can follow.
                if let Some(name) = tag.strip_prefix("v ").or_else(|| tag.strip_prefix("v.")) {
                    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
                    if !name.is_empty() {
                        out.push_str(&name);
                        out.push_str(": ");
                    }
                }
            }
            '&' => {
                let mut entity = String::new();
                let mut closed = false;
                while let Some(&inner) = chars.peek() {
                    chars.next();
                    if inner == ';' {
                        closed = true;
                        break;
                    }
                    entity.push(inner);
                    if entity.len() > 8 {
                        break;
                    }
                }
                match (closed, entity.as_str()) {
                    (true, "amp") => out.push('&'),
                    (true, "lt") => out.push('<'),
                    (true, "gt") => out.push('>'),
                    (true, "nbsp") => out.push(' '),
                    (true, "lrm" | "rlm") => {}
                    // Not one of WebVTT's five, so it was never an entity: put
                    // back what was actually written rather than swallowing it.
                    (_, other) => {
                        out.push('&');
                        out.push_str(other);
                        if closed {
                            out.push(';');
                        }
                    }
                }
            }
            other => out.push(other),
        }
    }
    out
}

/// Whether a bare `#name` selector would mean this id.
///
/// The CSS grammar for an identifier, in the subset that matters: no leading
/// digit, and nothing that would end the identifier and start something else.
/// A `.` makes `#a.b` an id *and* a class, a space makes it a descendant
/// combinator, and either way the selector matches an element that is not the
/// one it was minted for.
fn is_css_ident(id: &str) -> bool {
    // css-syntax-3's own rule rather than an approximation of it. Every code
    // point at or above U+0080 is an identifier code point, which is what makes
    // `café` in either normal form work, and Devanagari, and every combining
    // mark — an earlier version tested `is_alphanumeric` instead and rejected
    // all of those, dropping a perfectly good handle on the ground.
    fn starts(c: char) -> bool {
        c.is_alphabetic() || c == '_' || c >= '\u{80}'
    }
    fn continues(c: char) -> bool {
        starts(c) || c.is_ascii_digit() || c == '-'
    }

    let mut chars = id.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    // A leading `-` starts an identifier when a letter, `_` or a second `-`
    // follows it: `--foo` is as valid as `-foo`, and `#-1a` and a bare `-` are
    // not identifiers at all.
    let start_ok = if first == '-' {
        chars.clone().next().is_some_and(|c| starts(c) || c == '-')
    } else {
        starts(first)
    };
    start_ok && id.chars().all(continues)
}

fn attr(node: &Node, name: &str) -> Option<String> {
    node.element_data()?
        .attrs
        .iter()
        .find(|a| &*a.name.local == name)
        .map(|a| a.value.to_string())
}

/// Absolute, or nothing.
///
/// A relative URL handed back as written is useless to every caller: the CLI
/// prints it, an agent fetches it against the wrong base, and the broker cannot
/// even parse it. Failing to resolve is therefore the same answer as having no
/// source at all.
fn resolve(base: &url::Url, raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    base.join(raw).ok().map(|url| url.to_string())
}

fn cap(mut value: String, limit: usize) -> String {
    if value.len() <= limit {
        return value;
    }
    let mut at = limit;
    while at > 0 && !value.is_char_boundary(at) {
        at -= 1;
    }
    value.truncate(at);
    value.push_str(" …[truncated]");
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{PageFactory, PageOptions};
    use crate::policy::Policy;
    use crate::receipt::MemorySink;
    use std::sync::Arc;

    const PAGE: &str = "https://site.example/talk";

    fn discovered(html: &str) -> Transcript {
        let broker =
            crate::net::LocalBroker::new(Policy::new(), Arc::new(MemorySink::new()), None)
                .expect("broker");
        let fonts = crate::fonts::load(&[], &crate::fonts::default_font_dirs(), Some(2));
        let factory = PageFactory::new(broker, fonts.sources.clone(), PageOptions::default());
        let base = url::Url::parse(PAGE).unwrap();
        let page = factory.from_html(html, &base);
        let dom = page.dom();
        let doc = dom.borrow();
        discover(&doc, &base)
    }

    #[test]
    fn a_video_with_a_caption_track_is_found_and_its_urls_are_absolute() {
        let found = discovered(
            r#"<html><body>
                 <video src="/media/talk.mp4" title="The talk">
                   <track kind="captions" srclang="en" label="English" default src="/cc/en.vtt">
                   <track kind="subtitles" srclang="fr" label="Français" src="/cc/fr.vtt">
                 </video>
               </body></html>"#,
        );

        assert_eq!(found.media.len(), 1);
        let media = &found.media[0];
        assert_eq!(media.kind, "video");
        assert_eq!(media.src.as_deref(), Some("https://site.example/media/talk.mp4"));
        assert_eq!(media.label.as_deref(), Some("The talk"));
        assert_eq!(media.tracks.len(), 2);
        assert_eq!(media.selector, None, "this element has no id, so it has no handle");
        assert_eq!(media.tracks[0].src, "https://site.example/cc/en.vtt");
        assert!(media.tracks[0].default);
        assert!(!media.tracks[1].default);
        assert!(!media.is_silent());
    }

    /// The fact that routes a caller to another lane. Reported, not inferred
    /// from an empty list: "there is a video here and it ships no captions" is
    /// a different answer from "there is no video".
    #[test]
    fn media_with_no_track_is_reported_as_silent_rather_than_omitted() {
        let found = discovered(r#"<html><body><audio src="/ep/12.mp3"></audio></body></html>"#);

        assert_eq!(found.media.len(), 1);
        assert_eq!(found.media[0].kind, "audio");
        assert!(found.media[0].is_silent());
        assert_eq!(found.silent().len(), 1);
        assert!(!found.is_empty(), "the page has media; it has no captions");
    }

    /// A handle that resolves to the wrong element is worse than no handle,
    /// so the only selector minted here is one the page itself made durable.
    #[test]
    fn an_id_becomes_a_selector_and_a_position_does_not() {
        let found = discovered(
            r#"<html><body>
                 <div><video id="player" src="/a.mp4"><track src="/a.vtt"></video></div>
                 <div><video src="/b.mp4"><track src="/b.vtt"></video></div>
               </body></html>"#,
        );
        assert_eq!(found.media.len(), 2);
        assert_eq!(found.media[0].selector.as_deref(), Some("#player"));
        assert_eq!(found.media[1].selector, None);
    }

    /// `#dup` names the first element with that id, and duplicate ids are
    /// legal. Handing the second one back as `#player` gives a caller a handle
    /// that acts on the first.
    #[test]
    fn a_duplicated_or_unusable_id_is_not_offered_as_a_handle() {
        let found = discovered(
            r#"<html><body>
                 <video id="player" src="/a.mp4"><track src="/a.vtt"></video>
                 <video id="player" src="/b.mp4"><track src="/b.vtt"></video>
                 <video id="video.main" src="/c.mp4"><track src="/c.vtt"></video>
                 <video id="ok" src="/d.mp4"><track src="/d.vtt"></video>
               </body></html>"#,
        );
        assert_eq!(found.media.len(), 4);
        assert_eq!(found.media[0].selector, None, "duplicated id");
        assert_eq!(found.media[1].selector, None, "duplicated id");
        assert_eq!(
            found.media[2].selector, None,
            "`#video.main` is an id plus a class, and matches something else"
        );
        assert_eq!(found.media[3].selector.as_deref(), Some("#ok"));
    }

    #[test]
    fn is_css_ident_refuses_what_would_end_the_identifier() {
        assert!(is_css_ident("player"));
        assert!(is_css_ident("main-video_2"));
        assert!(is_css_ident("café"));
        assert!(is_css_ident("-leading-dash-then-letter"));
        assert!(is_css_ident("--custom-property-shaped"), "a second dash also starts one");
        assert!(is_css_ident("नमस्ते"), "every code point above U+0080 is one");
        assert!(is_css_ident("cafe\u{301}"), "and so is a combining mark");
        assert!(!is_css_ident("video.main"), "a class follows the id");
        assert!(!is_css_ident("two words"), "a descendant combinator");
        assert!(!is_css_ident("2fast"), "an identifier cannot start with a digit");
        assert!(!is_css_ident("-1a"), "nor a dash and then one");
        assert!(!is_css_ident("-"), "a bare dash is not an identifier");
        // A no-break space is whitespace, so `collapse` has already turned it
        // into a real space by the time an id reaches here, and the space is
        // what this rejects.
        assert!(!is_css_ident("play er"));
        assert!(!is_css_ident(""));
    }

    /// The bound treated a legitimately long *closed* tag as unterminated and
    /// put the markup verbatim into the transcript — the same mistake as
    /// eating the cue, pointing the other way.
    /// The quadratic version of this hung on input it is handed directly: a
    /// caption body with no blank line is one cue, `MAX_CUE_BYTES` is applied
    /// to what `strip_markup` *returns*, and a fresh copy of the remainder per
    /// `<` is tens of gigabytes at this size. It finishes instantly or it does
    /// not finish.
    #[test]
    fn many_unterminated_tags_do_not_take_quadratic_time() {
        let body = "< ".repeat(60_000);
        let (cues, _) = parse(
            &format!("WEBVTT\n\n00:00:01.000 --> 00:00:02.000\n{body}\n"),
            DEFAULT_MAX_BYTES,
        );
        assert_eq!(cues.len(), 1);
    }

    #[test]
    fn a_long_but_closed_tag_is_still_stripped() {
        let long_class = "a-generated-class-name".repeat(12);
        let (cues, _) = parse(
            &format!(
                "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\n<c.{long_class}>the words</c>\n"
            ),
            DEFAULT_MAX_BYTES,
        );
        assert_eq!(cues[0].text, "the words", "{}", cues[0].text);
    }

    /// A track whose `src` will not parse gets an error before it is ever
    /// marked fetched, and requiring both let it fall through to "its words
    /// exist only in the audio".
    #[test]
    fn a_track_that_never_reached_the_wire_is_still_a_read_failure() {
        let mut transcript = Transcript {
            media: vec![Media {
                kind: "video".into(),
                selector: None,
                src: None,
                label: None,
                tracks: vec![Track {
                    kind: "captions".into(),
                    language: None,
                    label: None,
                    default: true,
                    src: "not a url".into(),
                    fetched: false,
                    seq: None,
                    error: Some("unreadable track URL: relative URL".into()),
                    cues: Vec::new(),
                    truncated: None,
                }],
            }],
            notes: Vec::new(),
        };
        assert_eq!(transcript.read_failures().len(), 1);

        // A track that was listed and never asked for is not a failure.
        transcript.media[0].tracks[0].error = None;
        assert!(transcript.read_failures().is_empty());
    }

    /// An unterminated `<` is a literal in out-of-spec text, and routine in
    /// scraped caption files. Scanning to end of input discarded the cue.
    #[test]
    fn an_unterminated_tag_does_not_eat_the_rest_of_the_cue() {
        let (cues, _) = parse(
            "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\n5 < 6 and that is the whole of it\n",
            DEFAULT_MAX_BYTES,
        );
        assert_eq!(cues.len(), 1);
        assert!(cues[0].text.contains("whole of it"), "{}", cues[0].text);
    }

    /// `--lang` is only a lever for the kinds this verb reads, so offering it
    /// for a `metadata` track sends a caller round a loop that cannot end.
    #[test]
    fn a_track_this_verb_never_reads_is_not_offered_a_language_flag() {
        let mut found = discovered(
            r#"<html><body><video src="/v.mp4">
                 <track kind="captions" srclang="en" default src="/cc/en.vtt">
                 <track kind="metadata" srclang="en" src="/cc/m.vtt">
               </video></body></html>"#,
        );
        // As `read` would leave them: the captions fetched, the metadata not.
        found.media[0].tracks[0].fetched = true;
        found.media[0].tracks[0].cues = vec![Cue {
            start: 1.0,
            end: 2.0,
            text: "words".into(),
        }];

        let rendered = found.render("https://site.example/");
        assert!(rendered.contains("not a transcript"), "{rendered}");
        assert_eq!(
            rendered.matches("Name its language with `--lang`").count(),
            0,
            "the metadata track has no language that would help: {rendered}"
        );
    }

    #[test]
    fn a_page_with_no_media_at_all_is_empty() {
        assert!(discovered("<html><body><p>words</p></body></html>").is_empty());
    }

    /// `<source>` is how a page offers one media in several codecs, and an
    /// element that uses them has no `src` of its own.
    #[test]
    fn a_source_child_supplies_the_media_url() {
        let found = discovered(
            r#"<html><body><video>
                 <source src="/talk.webm" type="video/webm">
                 <source src="/talk.mp4" type="video/mp4">
                 <track src="/cc/en.vtt">
               </video></body></html>"#,
        );
        assert_eq!(
            found.media[0].src.as_deref(),
            Some("https://site.example/talk.webm")
        );
    }

    /// The standard's own missing-value default, and the classic way a track
    /// gets dropped from a transcript for omitting an attribute it was allowed
    /// to omit.
    #[test]
    fn a_track_with_no_kind_is_subtitles() {
        let found = discovered(
            r#"<html><body><video><track srclang="en" src="/cc/en.vtt"></video></body></html>"#,
        );
        assert_eq!(found.media[0].tracks[0].kind, "subtitles");
        assert!(found.media[0].tracks[0].carries_text());
    }

    #[test]
    fn an_unrecognised_kind_falls_back_the_same_way() {
        assert_eq!(normalise_kind(Some("nonsense")), "subtitles");
        assert_eq!(normalise_kind(None), "subtitles");
        assert_eq!(normalise_kind(Some("CAPTIONS")), "captions");
        assert_eq!(normalise_kind(Some("metadata")), "metadata");
    }

    /// `default=""` and `default="false"` both mean the attribute is present,
    /// which in HTML means true. Reading the value is how this gets inverted.
    #[test]
    fn default_is_presence_and_not_the_value() {
        let found = discovered(
            r#"<html><body><video><track default="false" src="/cc/en.vtt"></video></body></html>"#,
        );
        assert!(found.media[0].tracks[0].default);
    }

    #[test]
    fn a_track_with_no_src_declares_nothing_readable_and_is_skipped() {
        let found = discovered(
            r#"<html><body><video src="/t.mp4"><track kind="captions" srclang="en"></video></body></html>"#,
        );
        assert!(found.media[0].tracks.is_empty());
        assert!(found.media[0].is_silent());
    }

    // --- selection ---

    fn media_with(tracks: &[(&str, &str, bool)]) -> Media {
        Media {
            kind: "video".into(),
            selector: None,
            src: None,
            label: None,
            tracks: tracks
                .iter()
                .map(|(kind, lang, default)| Track {
                    kind: kind.to_string(),
                    language: Some(lang.to_string()),
                    label: None,
                    default: *default,
                    src: format!("https://site.example/cc/{lang}.vtt"),
                    fetched: false,
                    seq: None,
                    error: None,
                    cues: Vec::new(),
                    truncated: None,
                })
                .collect(),
        }
    }

    #[test]
    fn one_track_per_element_by_default_and_the_pages_own_default_wins() {
        let media = media_with(&[("subtitles", "fr", false), ("captions", "en", true)]);
        assert_eq!(chosen(&media, &Selection::default()), vec![1]);
    }

    #[test]
    fn a_named_language_beats_the_pages_default_and_matches_by_prefix() {
        let media = media_with(&[("captions", "en-GB", false), ("subtitles", "de", true)]);
        let selection = Selection {
            language: Some("en".into()),
            ..Selection::default()
        };
        assert_eq!(chosen(&media, &selection), vec![0], "en finds en-GB");
    }

    /// The outline is read alongside the words, because it is different
    /// information rather than a translation of the same information.
    /// `metadata` is machine payload the page reads with script, and
    /// `descriptions` is for a screen reader; neither is a transcript.
    #[test]
    fn one_spoken_track_and_the_outline_beside_it() {
        let media = media_with(&[
            ("captions", "en", false),
            ("metadata", "en", false),
            ("chapters", "en", false),
            ("descriptions", "en", false),
        ]);
        assert_eq!(chosen(&media, &Selection::default()), vec![0, 2]);
    }

    /// One language of the words, not thirty. A well-localised player declares
    /// thirty and they are the same words thirty times.
    #[test]
    fn only_one_language_of_the_spoken_track_is_read() {
        let media = media_with(&[
            ("captions", "en", true),
            ("subtitles", "de", false),
            ("subtitles", "fr", false),
        ]);
        assert_eq!(chosen(&media, &Selection::default()), vec![0]);
    }

    /// And the outline follows the language too, rather than being taken from
    /// whichever chapters track happens to come first.
    #[test]
    fn the_outline_follows_the_language_that_was_asked_for() {
        let media = media_with(&[
            ("captions", "en", true),
            ("chapters", "en", false),
            ("chapters", "de", false),
        ]);
        let selection = Selection {
            language: Some("de".into()),
            ..Selection::default()
        };
        // No German words on this page, so the English captions stand; the
        // German outline is the one that was asked for and exists.
        assert_eq!(chosen(&media, &selection), vec![0, 2]);
    }

    #[test]
    fn an_element_whose_only_track_is_metadata_selects_nothing() {
        let media = media_with(&[("metadata", "en", true)]);
        assert!(chosen(&media, &Selection::default()).is_empty());
        assert!(media.is_silent());
    }

    // --- parsing ---

    #[test]
    fn webvtt_parses_to_timed_cues() {
        let (cues, truncated) = parse(
            "WEBVTT\n\n1\n00:00:01.000 --> 00:00:04.000\nHello there.\n\n\
             2\n00:01:05.500 --> 00:01:07.000 line:0 position:20%\nGeneral Kenobi.\n",
            DEFAULT_MAX_BYTES,
        );
        assert!(truncated.is_none());
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].start, 1.0);
        assert_eq!(cues[0].end, 4.0);
        assert_eq!(cues[0].text, "Hello there.");
        assert_eq!(cues[1].start, 65.5);
        assert_eq!(cues[1].text, "General Kenobi.", "cue settings are not text");
    }

    /// The hours field is optional in WebVTT, so the parts are counted from the
    /// right. Reading them left to right turns `05:00.000` into five hours.
    #[test]
    fn a_timestamp_without_hours_is_minutes_and_seconds() {
        assert_eq!(seconds("05:00.000"), Some(300.0));
        assert_eq!(seconds("01:05:00.000"), Some(3900.0));
        assert_eq!(seconds("00:00:02,500"), Some(2.5), "SRT's comma");
        assert_eq!(seconds("nonsense"), None);
        assert_eq!(seconds("-1:00.000"), None);
    }

    /// One reader for both formats: SRT is blank-line-separated blocks with a
    /// numeric id and a comma, which the cue-id line and the comma swap already
    /// handle.
    #[test]
    fn srt_reads_through_the_same_parser() {
        let (cues, _) = parse(
            "1\n00:00:01,000 --> 00:00:02,000\nFirst.\n\n2\n00:00:03,000 --> 00:00:04,000\nSecond.\n",
            DEFAULT_MAX_BYTES,
        );
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[1].text, "Second.");
    }

    #[test]
    fn crlf_and_a_bom_do_not_break_the_first_cue() {
        let (cues, _) = parse(
            "\u{feff}WEBVTT\r\n\r\n00:00:01.000 --> 00:00:02.000\r\nFirst.\r\n",
            DEFAULT_MAX_BYTES,
        );
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].text, "First.");
    }

    /// STYLE would otherwise arrive as a cue full of CSS selectors.
    #[test]
    fn note_style_and_region_blocks_are_not_cues() {
        let (cues, _) = parse(
            "WEBVTT\n\nNOTE this is a comment\nwith a second line\n\n\
             STYLE\n::cue { color: yellow }\n\n\
             REGION\nid:speaker width:40%\n\n\
             00:00:01.000 --> 00:00:02.000\nActual words.\n",
            DEFAULT_MAX_BYTES,
        );
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].text, "Actual words.");
    }

    #[test]
    fn cue_markup_is_stripped_and_the_speaker_is_kept() {
        let (cues, _) = parse(
            "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\n\
             <v Roger Bannister>It was <i>fast</i> &amp; clean.\n",
            DEFAULT_MAX_BYTES,
        );
        assert_eq!(cues[0].text, "Roger Bannister: It was fast & clean.");
    }

    /// Not one of WebVTT's five entities, so it was never an entity.
    #[test]
    fn a_bare_ampersand_survives_as_itself() {
        let (cues, _) = parse(
            "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nR&D and Q&A;\n",
            DEFAULT_MAX_BYTES,
        );
        assert_eq!(cues[0].text, "R&D and Q&A;");
    }

    /// The fence rests on no page-derived value spanning a line, and a caption
    /// file is a stranger's bytes exactly as a heading is.
    #[test]
    fn a_cue_cannot_span_a_line() {
        let (cues, _) = parse(
            "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nfirst\nsecond\n",
            DEFAULT_MAX_BYTES,
        );
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].text, "first second");
        assert!(!cues[0].text.contains('\n'));
    }

    #[test]
    fn an_escape_sequence_in_a_cue_does_not_reach_the_terminal() {
        let (cues, _) = parse(
            "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nplain \u{1b}[2Jforged\n",
            DEFAULT_MAX_BYTES,
        );
        assert!(!cues[0].text.contains('\u{1b}'));
    }

    /// A partial transcript that does not say it is partial is worse than a
    /// short one: a model handed the first ten minutes with no note summarises
    /// it as the whole talk.
    #[test]
    fn a_transcript_over_the_byte_ceiling_says_where_it_stopped() {
        let mut vtt = String::from("WEBVTT\n\n");
        for i in 0..200 {
            vtt.push_str(&format!(
                "00:00:{:02}.000 --> 00:00:{:02}.000\n{}\n\n",
                i % 60,
                (i + 1) % 60,
                "word ".repeat(20)
            ));
        }
        let (cues, truncated) = parse(&vtt, 512);
        assert!(cues.len() < 200);
        assert!(truncated.unwrap().contains("512"));
    }

    #[test]
    fn empty_input_parses_to_nothing_rather_than_failing() {
        assert_eq!(parse("", DEFAULT_MAX_BYTES).0.len(), 0);
        assert_eq!(parse("WEBVTT\n", DEFAULT_MAX_BYTES).0.len(), 0);
    }

    #[test]
    fn the_stamp_grows_an_hours_field_only_when_there_are_hours() {
        assert_eq!(stamp(0.0), "00:00");
        assert_eq!(stamp(65.4), "01:05");
        assert_eq!(stamp(3661.0), "01:01:01");
    }

    #[test]
    fn the_text_rendering_carries_the_clock_so_a_reader_can_cite_it() {
        let track = Track {
            kind: "captions".into(),
            language: None,
            label: None,
            default: true,
            src: "https://site.example/cc/en.vtt".into(),
            fetched: true,
            seq: Some(7),
            error: None,
            cues: vec![
                Cue { start: 1.0, end: 2.0, text: "First.".into() },
                Cue { start: 760.0, end: 762.0, text: "Later.".into() },
            ],
            truncated: None,
        };
        assert_eq!(track.text(), "[00:01] First.\n[12:40] Later.");
    }
}
