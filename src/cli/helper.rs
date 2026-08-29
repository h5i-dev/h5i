//! The helper lane: an outside program, run deliberately, recorded as one.
//!
//! `h5i browser transcript` reads the captions a page declares (`<track>`), and
//! that covers a documentation site, a conference recording, a news video and
//! most of the accessible web. It does not cover the sites where the captions
//! are not in the markup at all — YouTube being the one that matters most,
//! whose transcript lives behind the player's own JSON API and is reachable
//! only by a program that knows how that API works.
//!
//! yt-dlp is that program, for about 1,700 sites. This module runs it.
//!
//! # Why this is a lane and not a feature
//!
//! The engine's request log carries one claim: *a request that is not in it did
//! not happen*. That claim is worth something because the engine **is** the
//! HTTP client — policy, receipt, wire, in that order, with no second path.
//!
//! A helper is a second path. It opens its own sockets from a process the
//! engine never sees, so its fetches are not in `h5i browser requests` and
//! cannot be. Two ways to handle that, and only one of them is honest:
//!
//! * Write the helper's fetches into the engine's log. This is the tempting
//!   one, and it is a lie: the engine did not decide about those requests and
//!   did not see them, so a log that listed them would be an *observation*
//!   dressed as a decision record. The whole product is arranged against
//!   exactly that.
//! * Keep it out of that log, say so, and record it somewhere else. This.
//!
//! So the lane is opt-in at every layer — a cargo feature, then an explicit
//! `--via yt-dlp` that never fires by default and never fires as a fallback —
//! and every run appends a **host-observed** row to
//! [`bs::HELPERS_FILE`](h5i_core::browser_session::HELPERS_FILE) naming the
//! program and the exact argv. h5i built that argv, so it is a fact rather than
//! the helper's account of itself, and `h5i browser audit` renders it in the
//! timeline beside the engine's own rows with the lane marked.
//!
//! # What contains it
//!
//! The session's placement, and nothing else here. A boxed session runs the
//! helper **inside its box**; a host session runs it on the host, where it is
//! contained by whatever the shell that started h5i is contained by, which is
//! to say by nothing this module can name.
//!
//! Whether being in a box buys a *network* boundary is a second question, and
//! [`evidence`] answers it from the session's lane rather than assuming. A box
//! confines files and environment at every tier and egress at only some, and on
//! Linux today the tiers that enforce egress cannot hold a resident browser
//! session at all — so a boxed session is on `workspace` or `process`, neither
//! of which polices what leaves it. Reporting that as containment would be the
//! generous-direction error [`h5i_core::browser_session::Session::lane_for`]
//! exists to prevent.
//!
//! **There is no fallback between the two.** A boxed session whose box has no
//! yt-dlp is refused rather than served from the host: running it outside would
//! move the session's network to a boundary the caller did not choose, which is
//! a security-policy change wearing the clothes of a convenience (ROADMAP's
//! rule for the second engine, and the same reasoning).
//!
//! # What it is not given
//!
//! No credential. `--secret` grants are resolved by the broker on the way into
//! a *page*, and this lane has no page and no broker; the environment the child
//! gets is built here from a short list and does not include the
//! `H5I_SECRET_*` namespace. `--ignore-config` is passed for the same class of
//! reason: a `~/.config/yt-dlp/config` on the host could otherwise add flags
//! h5i did not choose, which would make the recorded argv untrue.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use h5i_core::browser_session as bs;
use serde_json::{json, Value};

/// The one helper there is, and the name the lane is spelled with.
pub const NAME: &str = "yt-dlp";

/// Where to look, when `PATH` does not answer.
///
/// The two paths a package manager and a `pip install --user` land in. Absolute
/// rather than relative, so a `yt-dlp` in the working directory is never what
/// runs — this module execs a program named by a flag, and picking one up from
/// wherever h5i happens to be standing is how that becomes a way to run
/// something else entirely.
const FALLBACKS: &[&str] = &["/usr/local/bin/yt-dlp", "/usr/bin/yt-dlp"];

/// How long the helper may run before it is killed.
///
/// A transcript is a metadata fetch and a subtitle file. Two minutes is
/// generous for both on a slow link and is nothing like long enough for the
/// media download this lane never asks for.
const BUDGET: Duration = Duration::from_secs(120);

/// Override for [`BUDGET`], in whole seconds.
///
/// Two readers. A slow or rate-limited link where two minutes is genuinely not
/// enough, and a test that needs the stop path to happen this second rather
/// than in two minutes — which is the only way to exercise it at all, and it
/// went untested until it was.
///
/// Clamped rather than trusted: zero would stop every run before it began, and
/// a very large value would turn the budget into no budget, which is the thing
/// it exists not to be.
const BUDGET_ENV: &str = "H5I_HELPER_BUDGET_SECS";

fn budget() -> Duration {
    budget_from(std::env::var(BUDGET_ENV).ok().as_deref())
}

/// Split from [`budget`] so the clamping is testable without an environment.
/// A test that sets a process-wide variable is a test that races every other
/// test in the binary.
fn budget_from(raw: Option<&str>) -> Duration {
    match raw.and_then(|v| v.trim().parse::<u64>().ok()) {
        Some(secs) => Duration::from_secs(secs.clamp(1, 1800)),
        None => BUDGET,
    }
}

/// The environment the child gets, and the whole of it.
///
/// An allowlist rather than a filter: a filter has to know every variable worth
/// removing, and the list that matters here is the three or four worth keeping.
/// `H5I_SECRET_*` is absent by construction rather than by exclusion, which is
/// the difference between a rule and a hope.
const KEEP: &[&str] = &[
    "PATH",
    "HOME",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TMPDIR",
    // The box's egress proxy, when there is one. Passed on purpose: it is what
    // puts the helper's traffic through the same boundary as everything else.
    "HTTPS_PROXY",
    "https_proxy",
    "HTTP_PROXY",
    "http_proxy",
    "NO_PROXY",
    "no_proxy",
];

/// What the lane produced, before it is rendered.
pub struct Outcome {
    pub argv: Vec<String>,
    pub status: Option<i32>,
    pub reply: Value,
    /// One line for the audit row: what came of it.
    pub note: String,
    /// Whether a transcript actually arrived.
    ///
    /// The field a caller decides its exit code on, and deliberately not
    /// `status`: yt-dlp exits non-zero if any part of a run failed, so a run
    /// that wrote the transcript and then hit a rate limit on something else
    /// looks like a failure and is not one. What arrived decides.
    pub answered: bool,
}

/// Read a transcript with the helper.
///
/// `url` is the media to read. It is **not** navigated to first, which is the
/// one place this lane's flags differ in meaning from the engine's: the engine
/// reads the page it is on, and the helper reads a URL directly, so moving the
/// session to a page nobody is going to read would be a page load spent on
/// nothing.
pub fn transcript(
    root: &Path,
    session: &bs::Session,
    url: &str,
    lang: Option<&str>,
    all: bool,
    max_bytes: usize,
) -> anyhow::Result<Outcome> {
    let url = check_url(url)?;
    let work = workspace(root, session)?;
    scrub(&work)?;

    // The default is written down once, here, and used for **both** the pattern
    // yt-dlp is given and the file chosen out of what comes back. Deriving it
    // twice is what went wrong the first time this ran against a real video.
    //
    // **Exact, not a prefix.** yt-dlp matches `--sub-langs` with a regex
    // `fullmatch`, so `en` means `en` and `en.*` means every tag beginning with
    // it. Against YouTube that widening is not a convenience, it is three
    // downloads instead of one: a video's `en.*` is `en` plus `en-de` and
    // `en-en`, the author's captions and two machine translations of them. The
    // first live run of this lane asked that way, got rate-limited on the third
    // file, lost the metadata that is written after the last one, and returned
    // a translation of a translation. One tag, one file.
    //
    // The value is still a regex, so a caller who wants the widening has it:
    // `--lang 'en.*'` is the escape hatch, and a run that matched nothing names
    // it along with the tags the video actually has.
    let want = lang.unwrap_or("en");
    // `--all` means the same thing on both lanes: every track the media
    // actually carries. On the `<track>` lane that is what the markup declares,
    // one to a few dozen. Here it has to be said with two flags rather than
    // one, because yt-dlp folds machine translations into `all` the moment
    // `--write-auto-subs` is on — three hundred languages on an ordinary
    // YouTube video, and nine hundred on a popular one.
    //
    // So `--all` drops the automatic flag and asks only for the tracks somebody
    // wrote. A single language still asks for both, because a video whose only
    // transcript is machine-generated is exactly the case this lane exists to
    // reach — and that asymmetry is why `--all` needs the second pass below:
    // without it, `--all` on such a video returns nothing while passing no flag
    // at all returns the transcript, and a flag that asks for more must never
    // hand back less.
    let langs = if all {
        // `live_chat` is a chat transcript rather than a subtitle track, and on
        // a long stream it is enormous. Excluded by name, the way yt-dlp's own
        // documentation excludes it.
        "all,-live_chat".to_string()
    } else {
        want.to_string()
    };

    let mut argv = build_argv(&url, &langs, &work.in_helper, !all);
    let (mut status, mut said) = run(session, &argv, &work)?;
    // `want` goes to yt-dlp as a **regex** and to `collect` as a literal, so
    // the chooser gets the literal head of it. Without this the escape hatch
    // this lane recommends by name — `--lang 'en.*'`, printed by `describe` —
    // matched nothing in the chooser: no tag equals or starts with `en.*`, so
    // selection fell through to whatever sorted first, and `-` sorts before `.`
    // so `en-de` won. The translation-of-a-translation bug, handed to a caller
    // who followed this tool's own advice.
    let mut read = collect(&work.host, Some(literal_prefix(want)), max_bytes, all);
    record(root, session, &work, &argv, status, &describe(&read, want, status, &said));

    // The second pass, and only for `--all` on a video that turns out to have
    // no authored captions at all.
    //
    // The info file the first pass wrote is what makes this decidable without
    // guessing: asked for subtitles it could not supply, yt-dlp still writes it
    // and still lists what the video has, in both kinds.
    //
    // How long that automatic list is varies by video — some report only their
    // source tracks, others the whole translated set — and it deliberately does
    // not matter here, because `fallback_tag` takes exactly one tag out of it.
    // The bound is on what gets *asked for*, not on what gets listed, which is
    // the same bound `--all` dropping the automatic flag was buying.
    let mut fell_back = None;
    if all
        && read.cues.is_empty()
        && read.authored.is_empty()
        && let Some(tag) = fallback_tag(&read.automatic_tags, want)
    {
        argv = build_argv(&url, &tag, &work.in_helper, true);
        let (second_status, second_said) = run(session, &argv, &work)?;
        status = second_status;
        said = second_said;
        // The fallback fetched exactly one track, so there is nothing extra to
        // carry even though `--all` is what got us here.
        read = collect(&work.host, Some(literal_prefix(&tag)), max_bytes, false);
        record(
            root,
            session,
            &work,
            &argv,
            status,
            &describe(&read, &tag, status, &said),
        );
        fell_back = Some(tag);
    }

    let mut note = describe(&read, want, status, &said);
    if let Some(tag) = &fell_back {
        note.push_str(&format!(
            ". No authored captions on this video, so `--all` fell back to its automatic \
             transcript in `{tag}`."
        ));
    }
    // The box's own receipt for this invocation, when there is one. It is the
    // strongest evidence this lane produces — a row h5i wrote from outside the
    // helper, naming the policy the run was subject to — so the reply points at
    // it rather than leaving the two records to be matched by time.
    if let Some(receipt) = box_receipt(session, &work) {
        note.push_str(&format!(" Box receipt {receipt}."));
    }

    Ok(Outcome {
        reply: render(&url, session, &read, &note),
        // The last argv that ran. Each invocation has its own row in the helper
        // log, which is where the whole sequence is: a `--all` that fell back
        // ran twice, and a record showing one of them would be a record of a
        // command that is not the one that produced the answer.
        argv,
        status,
        answered: !read.cues.is_empty(),
        note,
    })
}

/// Append one row per invocation.
///
/// Per invocation rather than per verb, because the argv is the thing this row
/// exists to state and a `--all` that falls back runs two different ones.
/// Written whatever happened, including a failure: "h5i ran yt-dlp and it
/// exited 1" is the row an auditor needs most, and a log that only recorded
/// successes would report a quiet session where a program ran and failed.
fn record(
    root: &Path,
    session: &bs::Session,
    work: &Workspace,
    argv: &[String],
    status: Option<i32>,
    note: &str,
) {
    let mut note = note.to_string();
    if let Some(receipt) = box_receipt(session, work) {
        note.push_str(&format!(" Box receipt {receipt}."));
    }
    let _ = bs::record_helper(
        root,
        &session.id,
        &bs::HelperRow {
            // Stamped by `record_helper`, from the clock the audit sorts on.
            at: String::new(),
            name: NAME.to_string(),
            argv: argv.to_vec(),
            status,
            note: Some(note),
        },
    );
}

/// The literal head of a `--sub-langs` value.
///
/// yt-dlp matches that value as a regex and this module matches tags as text,
/// so a caller who writes `en.*` means "any tag starting with en" to one and a
/// tag literally named `en.*` to the other. Everything up to the first
/// metacharacter is the part both agree on.
fn literal_prefix(pattern: &str) -> &str {
    const META: &[char] = &[
        '.', '*', '+', '?', '[', ']', '(', ')', '{', '}', '|', '^', '$', '\\',
    ];
    match pattern.find(META) {
        // A pattern that is all metacharacter has no literal head, and a bare
        // `""` would prefix-match everything. Handing back the whole thing
        // matches nothing instead, which is the safe direction: the caller is
        // told what the video has rather than given an arbitrary track.
        Some(0) => pattern,
        Some(at) => &pattern[..at],
        None => pattern,
    }
}

/// Which automatic track to fall back to, out of the ones the video has.
///
/// Bounded to exactly one, because the whole point of `--all` dropping the
/// automatic flag is not to ask for hundreds. The order is what a caller would
/// expect: the language they named, then anything beginning with it, then a
/// bare language code — a tag with no `-` is a source transcript rather than a
/// translation of one, which is what `de-en` means — then whatever is first.
fn fallback_tag(automatic: &[String], want: &str) -> Option<String> {
    let want = want.to_ascii_lowercase();
    automatic
        .iter()
        .find(|tag| tag.eq_ignore_ascii_case(&want))
        .or_else(|| {
            automatic
                .iter()
                .find(|tag| tag.to_ascii_lowercase().starts_with(&want))
        })
        .or_else(|| automatic.iter().find(|tag| !tag.contains('-')))
        .or_else(|| automatic.first())
        .cloned()
}

/// Refuse a URL this lane should not be handed.
///
/// The scheme check is the whole of it, and it is not decoration: `--via` names
/// a program that takes a URL, and a `file://` or a shell-shaped string reaching
/// it is the difference between fetching a page and reading the host. The host
/// allowlist is deliberately **not** applied here — this lane's containment is
/// the session's placement, which [`transcript`] documents, and pretending to a
/// policy check that the engine and not this module owns would be worse than
/// having none.
fn check_url(raw: &str) -> anyhow::Result<String> {
    let trimmed = raw.trim();
    let parsed = url::Url::parse(trimmed)
        .map_err(|e| anyhow::anyhow!("`{trimmed}` is not a URL this lane can read: {e}"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        anyhow::bail!(
            "the helper lane reads `http` and `https` URLs, and `{}` is neither. \
             It fetches from the network; it does not read this machine.",
            parsed.scheme()
        );
    }
    Ok(parsed.to_string())
}

/// Start from a directory this run just made, or refuse.
///
/// **Not a tidy-up.** [`collect`] reads whatever `.vtt` and `.info.json` it
/// finds here and reports it as the transcript, so anything left in this
/// directory by anyone becomes an answer h5i hands a model with an `evidence:`
/// line and a box receipt beside it. The directory is host-global on a
/// workspace-tier box — that box's `/tmp` *is* the host's — so on a shared
/// machine any other user can create it first, plant a file, and make it
/// undeletable.
///
/// This used to be `let _ = remove_dir_all(...)` followed by `create_dir_all`,
/// which is exactly the wrong pair for that: the wipe's failure was discarded
/// and `create_dir_all` is happy with a directory that already exists, so a
/// planted transcript survived both calls and was read. `create_dir` refuses an
/// existing directory, which turns "somebody else owns this path" from a silent
/// read of their content into a refusal.
fn scrub(work: &Workspace) -> anyhow::Result<()> {
    match std::fs::remove_dir_all(&work.host) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => anyhow::bail!(
            "could not clear {} before running the helper: {e}. Anything left there would be \
             read as this run's transcript, so h5i will not run over it.",
            work.host.display()
        ),
    }
    if let Some(parent) = work.host.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::create_dir(&work.host).map_err(|e| {
        anyhow::anyhow!(
            "could not create {} for the helper: {e}. It must be a directory this run made, \
             because whatever is in it is read as the transcript.",
            work.host.display()
        )
    })
}

/// Where the helper writes, in both views of the filesystem.
#[derive(Debug)]
struct Workspace {
    /// The directory as **this machine** sees it, which is where the output is
    /// read back from.
    host: PathBuf,
    /// The output template as the **helper** sees it. The same directory for a
    /// host session, and the box's own path for a boxed one.
    in_helper: String,
}

/// Pick a directory both h5i and the helper can name.
///
/// For a host session that is the session directory, which h5i already owns.
/// For a boxed one it is beside the control socket in the box's `/tmp`, whose
/// two views the session record already carries — `control.file` is the box's
/// and `control.witness` is this machine's. Deriving them here rather than
/// re-computing the box's tmp mapping is the same rule the rest of this file
/// follows: two places that must agree about a path are two places that can
/// stop agreeing.
/// Pick a directory both h5i and the helper can name.
///
/// For a host session that is the session directory, which h5i already owns.
/// For a boxed one it is beside the control socket in the box's `/tmp`, whose
/// two views the session record already carries — `control.file` is the box's
/// and `control.witness` is this machine's. Deriving them here rather than
/// re-computing the box's tmp mapping is the same rule the rest of this file
/// follows: two places that must agree about a path are two places that can
/// stop agreeing.
///
/// (Doc kept with [`workspace`] below; [`leaf`] names the directory.)
///
/// The workspace directory's name, which has to carry the session id.
///
/// A bare `helper` is unique per session on the host, where the parent is the
/// session's own directory. In a box it is not: the parent is the box's `/tmp`,
/// and on a workspace-tier box that is the **host's** `/tmp`, so every box on
/// the machine and every process on it would share one `/tmp/helper`. Two boxes
/// reading transcripts at once would clear each other's output mid-run, and
/// anything a third party left there would be read as this run's answer.
///
/// The id rather than the name: a name can be reused once the session it named
/// has ended, which is what makes it comfortable to type and useless here.
fn leaf(session: &bs::Session) -> String {
    format!("helper-{}", session.id)
}

fn workspace(root: &Path, session: &bs::Session) -> anyhow::Result<Workspace> {
    match &session.placement {
        bs::Placement::Host => {
            // Already per-session here, since the parent is the session's own
            // directory. Named the same way anyway: one rule for where the
            // helper writes is one rule to check, and a reader comparing the
            // two arms should not have to work out whether the difference
            // matters.
            let dir = bs::dir(root, &session.id).join(leaf(session));
            Ok(Workspace {
                in_helper: dir.join("%(id)s").display().to_string(),
                host: dir,
            })
        }
        bs::Placement::Box { name } => {
            let in_box = session
                .control
                .file
                .as_ref()
                .and_then(|p| p.parent())
                .ok_or_else(|| {
                    anyhow::anyhow!("this session's record does not name the box's own /tmp")
                })?
                .join(leaf(session));
            // `None` means this machine cannot see the box's `/tmp` at all: an
            // image-backed tier is the designed reason, and a session that died
            // before its record was complete is the accidental one. Either way
            // it is refused rather than worked around, because the helper would
            // run, write somewhere h5i cannot read, and hand back a
            // successful-looking run that produced nothing.
            //
            // The message does not name a cause it cannot check. It used to say
            // "keeps its /tmp inside its image", which is one of the two and
            // reads as a fact about the tier — wrong, and confidently so, for a
            // workspace box whose record simply has no witness in it.
            let on_host = session
                .control
                .witness
                .as_ref()
                .and_then(|p| p.parent())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "h5i cannot see box `{name}`'s /tmp from here, so it cannot read back \
                         what a helper writes there. This session's record names no host-side \
                         view of it, which is how an image-backed tier looks and also how a \
                         session that never fully came up looks. Check `h5i browser status`, \
                         or read this URL with `--via` unset."
                    )
                })?
                .join(leaf(session));
            Ok(Workspace {
                in_helper: in_box.join("%(id)s").display().to_string(),
                host: on_host,
            })
        }
    }
}

/// The command line, built here and recorded exactly as built.
///
/// `automatic` decides whether machine transcriptions are in scope, and it is
/// what makes `all` mean the same thing here as it does on the `<track>` lane.
/// yt-dlp resolves `--sub-langs all` against the languages it is *willing to
/// write*, and `--write-auto-subs` is what puts several hundred machine
/// translations into that set (`YoutubeDL.py`, `available_subs`). Asking for
/// every language with automatic captions on is therefore not "every track this
/// video has", it is 314 requests on the first video this lane was pointed at
/// and 940 on the second: a rate limit within seconds, and nothing anybody
/// wanted.
fn build_argv(url: &str, langs: &str, out: &str, automatic: bool) -> Vec<String> {
    let mut argv: Vec<&str> = vec![
        // Nothing from a config file. A `~/.config/yt-dlp/config` could
        // otherwise add flags h5i did not choose — a proxy, a cookie file, a
        // post-processor — which would make the argv this module records an
        // incomplete account of what actually ran.
        "--ignore-config",
        // The media is never fetched. This lane reads what was *said*, and a
        // transcript verb that quietly pulled down a gigabyte of video would be
        // spending a box's network on something nobody asked for.
        "--skip-download",
        // A URL that resolves to a playlist would otherwise fetch every entry
        // in it. One URL, one media.
        "--no-playlist",
        "--no-warnings",
        "--no-progress",
        // Both, because the distinction matters to a reader and yt-dlp reports
        // which it wrote in the filename: an author's own captions and a
        // machine's transcription are different evidence, and `describe` says
        // which arrived.
        "--write-subs",
    ];
    if automatic {
        argv.push("--write-auto-subs");
    }
    argv.extend([
        "--sub-langs",
        langs,
        // WebVTT first because it is what the `<track>` lane parses, so both
        // lanes come out of one parser and cannot disagree about a timestamp.
        "--sub-format",
        "vtt/srt/best",
        // Title, duration, uploader and chapters, in one file, in the same
        // invocation. The alternative is a second `--print` run, which is a
        // second round trip to the same server to ask what it already said.
        "--write-info-json",
        "--socket-timeout",
        "20",
        "-o",
        out,
        // Everything after this is data. Without it a URL beginning with `-`
        // is read as a flag, which is the oldest argv bug there is.
        "--",
        url,
    ]);
    argv.iter().map(|s| s.to_string()).collect()
}

/// Run it where the session runs, and nowhere else.
fn run(
    session: &bs::Session,
    argv: &[String],
    work: &Workspace,
) -> anyhow::Result<(Option<i32>, String)> {
    let mut command = match &session.placement {
        bs::Placement::Host => {
            let binary = locate().ok_or_else(|| {
                anyhow::anyhow!(
                    "`{NAME}` is not on this machine's PATH. Install it (`pipx install yt-dlp`, \
                     or your package manager) and try again, or drop `--via` to read the \
                     captions the page itself declares."
                )
            })?;
            let mut command = Command::new(binary);
            command.args(argv);
            command
        }
        // Inside the box, so the box's egress enforcement sees this traffic.
        // Never on the host as a fallback: that would move the session's
        // network to a boundary its caller did not choose.
        bs::Placement::Box { name } => {
            let mut command = Command::new(std::env::current_exe()?);
            command
                .arg("box")
                .arg("run")
                .arg(name)
                .arg("--")
                .arg(NAME)
                .args(argv);
            command
        }
    };

    let out = std::fs::File::create(work.host.join("stdout.log"))?;
    let err_path = work.host.join("stderr.log");
    let err = std::fs::File::create(&err_path)?;
    command
        .env_clear()
        .envs(inherited())
        .stdin(Stdio::null())
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err));

    // Output to files rather than pipes, then poll: a pipe would have to be
    // drained on another thread to avoid filling and deadlocking the child,
    // and the files are wanted anyway — a helper that failed is a helper whose
    // stderr somebody is about to want.
    let mut child = command.spawn().map_err(|e| {
        anyhow::anyhow!("could not start `{NAME}`: {e}")
    })?;
    let budget = budget();
    let deadline = Instant::now() + budget;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait()? {
            Some(status) => break status.code(),
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                // Stopped, not failed. A run killed at the budget has usually
                // written some of what it was asked for, and this used to
                // return an error and throw all of it away — the same mistake
                // as reading yt-dlp's exit code instead of reading the
                // directory, and made in the one place where the caller has
                // already waited two minutes for the answer.
                timed_out = true;
                break None;
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    };

    let mut said = complaint(&session.placement, work);
    if timed_out {
        said.push_str(&format!(
            "\nh5i stopped `{NAME}` after {}s, its whole budget for one transcript.",
            budget.as_secs()
        ));
    }
    // `None` for a kill *and* for a signal death, which is why the timeout is
    // reported through the text rather than by an out-of-band status: what the
    // caller has to be able to tell apart is "nothing arrived" from "something
    // did", and the directory answers that.
    Ok((if timed_out { Some(-1) } else { status }, said))
}

/// The banner `h5i box run` puts between a child's two streams.
///
/// It merges them: the child's stderr arrives on `box run`'s **stdout**, after
/// this line, and `box run`'s own stderr carries only its receipt.
const BOX_RUN_STDERR_BANNER: &str = "----- stderr -----";

/// What the helper itself said when it went wrong, and nothing h5i said.
///
/// Two different files depending on where it ran, and getting this wrong is not
/// cosmetic. On the host the child's stderr is the child's stderr. Inside a box
/// it is not: `h5i box run` folds it into its own stdout under
/// [`BOX_RUN_STDERR_BANNER`] and keeps stderr for its receipt line, so reading
/// stderr there yields `◈ receipt … · exit 1 · wall 3ms` — h5i's own
/// bookkeeping, handed to a caller as the reason yt-dlp failed. It says nothing
/// about the failure and it reads as though h5i were the thing that broke.
fn complaint(placement: &bs::Placement, work: &Workspace) -> String {
    let read = |name: &str| std::fs::read_to_string(work.host.join(name)).unwrap_or_default();
    match placement {
        bs::Placement::Host => read("stderr.log"),
        bs::Placement::Box { .. } => {
            let merged = read("stdout.log");
            match merged.split_once(BOX_RUN_STDERR_BANNER) {
                Some((_, complaint)) => complaint.to_string(),
                // No banner means the child wrote nothing to stderr, which is
                // the whole of what this function is for. An empty answer is
                // right: `describe` falls back to saying it said nothing,
                // rather than reporting the child's *stdout* as a complaint.
                None => String::new(),
            }
        }
    }
}

/// The box receipt `h5i box run` wrote for this invocation.
///
/// Worth keeping, because it is the strongest evidence this lane produces: a
/// row in the box's own receipt log, written by h5i from outside the helper,
/// naming the policy the run was subject to. The audit row carries it so the
/// two records can be put beside each other.
fn box_receipt(session: &bs::Session, work: &Workspace) -> Option<String> {
    // Only where there *is* a box. On the host, `stderr.log` is yt-dlp's own
    // stderr, and any line of it carrying `receipt ` and eight hex digits would
    // have this claim a box receipt for a run that never entered one — a false
    // evidence claim, in the one lane whose entire value is evidence honesty.
    if !matches!(session.placement, bs::Placement::Box { .. }) {
        return None;
    }
    let text = std::fs::read_to_string(work.host.join("stderr.log")).ok()?;
    let at = text.find("receipt ")? + "receipt ".len();
    let id: String = text[at..]
        .chars()
        .take_while(|c| c.is_ascii_hexdigit())
        .collect();
    (id.len() >= 8).then_some(id)
}

/// `yt-dlp`, from `PATH` or from the two places an install lands.
fn locate() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join(NAME);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    FALLBACKS
        .iter()
        .map(PathBuf::from)
        .find(|candidate| candidate.is_file())
}

fn inherited() -> BTreeMap<String, String> {
    KEEP.iter()
        .filter_map(|key| {
            std::env::var(key)
                .ok()
                .filter(|value| !value.is_empty())
                .map(|value| (key.to_string(), value))
        })
        .collect()
}

/// What the helper left behind, read back.
#[derive(Default)]
struct Read {
    title: Option<String>,
    duration: Option<f64>,
    uploader: Option<String>,
    /// The subtitle file's own language tag, off its filename.
    language: Option<String>,
    /// Whether what arrived was a machine transcription rather than an author's
    /// captions. yt-dlp says so in the filename, and it is worth carrying: an
    /// automatic transcript mishears names, and a reader quoting one should
    /// know that before it quotes.
    automatic: bool,
    cues: Vec<h5i_browser_light::transcript::Cue>,
    truncated: Option<String>,
    /// The other tracks that arrived, parsed, in tag order.
    ///
    /// `--all` downloads every authored track, and reporting one of them made
    /// those requests pay for nothing: the tag list in the reply comes from the
    /// info file whether they were downloaded or not, so a caller learned
    /// nothing from the downloads that it did not already know. The CLI help
    /// says "read every text track", and this is that.
    extra: Vec<Extra>,
    /// The language tags this video *has*, from the info file rather than from
    /// what happened to be downloaded.
    ///
    /// The difference matters now that one tag means one file: a run that
    /// matched nothing would otherwise have nothing to say about why, and
    /// "there are no captions" and "there are captions, in tags you did not
    /// ask for" are the two answers a caller must be able to tell apart.
    ///
    /// Authored captions first and separately, because YouTube lists a couple
    /// of hundred machine translations beside them and burying the real ones in
    /// that list is the same as not reporting them.
    authored: Vec<String>,
    automatic_tags: Vec<String>,
}

/// Read back what the helper left behind.
///
/// `want` is the language the caller asked for. yt-dlp is given a *pattern*
/// (`en.*`), so more than one file can legitimately arrive — `en` and `en-orig`
/// both match — and picking the alphabetically first would hand back the
/// machine transcription when the author's own captions are sitting beside it.
///
/// `keep_extra` carries the tracks beyond the chosen one, and follows `--all`
/// rather than "did more than one file arrive": a `--lang` pattern also brings
/// back several, and the caller who wrote one asked for a language, not for
/// every rendering of it.
fn collect(dir: &Path, want: Option<&str>, max_bytes: usize, keep_extra: bool) -> Read {
    let mut read = Read::default();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return read;
    };
    let mut subtitles: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
        if name.ends_with(".info.json") {
            if let Ok(text) = std::fs::read_to_string(&path)
                && let Ok(info) = serde_json::from_str::<Value>(&text)
            {
                read.title = info["title"].as_str().map(str::to_string);
                read.duration = info["duration"].as_f64();
                read.uploader = info["uploader"].as_str().map(str::to_string);
                read.authored = tags(&info["subtitles"]);
                read.automatic_tags = tags(&info["automatic_captions"]);
            }
        } else if name.ends_with(".vtt") || name.ends_with(".srt") {
            subtitles.push(path);
        }
    }

    // Deterministic, because a directory listing is not: two runs of the same
    // command must produce the same transcript, or nothing downstream can be
    // compared between them.
    subtitles.sort();

    // The exact tag the caller asked for, then anything that starts with it,
    // then whatever arrived. `en` before `en-orig` falls out of this without a
    // special case: an exact match wins, and yt-dlp's automatic tracks always
    // carry a suffix.
    let chosen = want
        .and_then(|want| {
            let want = want.to_ascii_lowercase();
            subtitles
                .iter()
                .find(|p| tag_of(p).is_some_and(|t| t.eq_ignore_ascii_case(&want)))
                .or_else(|| {
                    subtitles.iter().find(|p| {
                        tag_of(p).is_some_and(|t| t.to_ascii_lowercase().starts_with(&want))
                    })
                })
        })
        .or_else(|| subtitles.first());

    if let Some(first) = chosen {
        read.language = tag_of(first);
        // yt-dlp's own spelling for an automatic track: the language tag it
        // writes for one carries the `-orig` or `a.` marker, and the info file
        // lists it under `automatic_captions`. The filename is the reliable
        // half across versions.
        // From the info file's two lists, not from the filename.
        //
        // The marker in a filename only appears on a *translated* automatic
        // track (`en-orig`, `a.en`). The case this lane exists to serve — a
        // video with automatic captions and no authored ones — gets written as
        // plain `<id>.en.vtt`, identical to an authored track. Reading the name
        // therefore said `automatic: false` exactly where the warning matters
        // most, and dropped "machine-transcribed, names are frequently wrong"
        // from the one transcript that needed it.
        read.automatic = read.language.as_deref().is_some_and(|tag| {
            let authored = read.authored.iter().any(|t| t.eq_ignore_ascii_case(tag));
            let automatic = read
                .automatic_tags
                .iter()
                .any(|t| t.eq_ignore_ascii_case(tag));
            // A tag in both lists is the author's own captions; yt-dlp prefers
            // those and so does this. Only a tag that is *only* automatic, or
            // one whose name says so, is reported as machine-transcribed.
            (automatic && !authored)
                || tag.contains("-orig")
                || tag.starts_with("a.")
        });
        if let Ok(text) = std::fs::read_to_string(first) {
            let (cues, truncated) = h5i_browser_light::transcript::parse(&text, max_bytes);
            read.cues = cues;
            read.truncated = truncated;
        }

        // Everything else that arrived, and only when every track was asked
        // for.
        //
        // `--all` is not the only thing that downloads more than one file: a
        // `--lang` **pattern** does too, and `en.*` against YouTube brings back
        // `en` and `en-en`, the author's captions and a machine translation of
        // English into English. Rendering both there is noise on top of the one
        // transcript the caller asked for. `--all` is the flag that says "every
        // text track", so it is the flag this follows.
        for path in subtitles.iter().filter(|p| *p != first).filter(|_| keep_extra) {
            let Ok(text) = std::fs::read_to_string(path) else {
                continue;
            };
            let (cues, truncated) = h5i_browser_light::transcript::parse(&text, max_bytes);
            if cues.is_empty() {
                continue;
            }
            read.extra.push(Extra {
                tag: tag_of(path),
                cues,
                truncated,
            });
        }
    }
    read
}

/// One track beyond the chosen one.
#[derive(Default)]
struct Extra {
    tag: Option<String>,
    cues: Vec<h5i_browser_light::transcript::Cue>,
    truncated: Option<String>,
}

/// The language tags out of an info file's `subtitles` / `automatic_captions`.
///
/// Both are objects keyed by tag. Sorted, because a JSON object's order is not
/// something to hand a reader as if it meant something.
fn tags(value: &Value) -> Vec<String> {
    let Some(map) = value.as_object() else {
        return Vec::new();
    };
    let mut out: Vec<String> = map.keys().map(|k| collapse(k)).filter(|k| !k.is_empty()).collect();
    out.sort();
    out
}

/// The language tag out of `<id>.<lang>.vtt`.
fn tag_of(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_string_lossy();
    let mut parts: Vec<&str> = name.split('.').collect();
    parts.pop()?;
    parts.pop().map(str::to_string)
}

/// One line saying what came of the run, for the audit row and the reply.
///
/// **What arrived decides, not the exit code.** yt-dlp exits non-zero if any
/// part of a run failed, and a run that wrote the transcript and then hit a 429
/// on something else is a run that answered the question. Reporting that as a
/// failure would send a caller retrying work it already has — while a caller
/// who is *not* told about the partial failure cannot explain the missing title
/// beside it, so the note carries both.
fn describe(read: &Read, want: &str, status: Option<i32>, stderr: &str) -> String {
    let reason = || {
        // The last line of stderr, which is where yt-dlp puts its reason.
        // Collapsed like every other value from outside: this reaches a model,
        // and a program's stderr is no more trusted than a page is.
        collapse(
            stderr
                .lines()
                .rev()
                .find(|line| !line.trim().is_empty())
                .unwrap_or("it said nothing"),
        )
    };

    if !read.cues.is_empty() {
        let mut note = format!(
            "{} cue(s){}{}",
            read.cues.len(),
            read.language
                .as_deref()
                .map(|l| format!(" in {l}"))
                .unwrap_or_default(),
            if read.automatic {
                ", automatic captions"
            } else {
                ""
            }
        );
        if status.is_some_and(|code| code != 0) {
            note.push_str(&format!(
                ". The transcript arrived and the rest of the run did not: {}",
                reason()
            ));
        }
        return note;
    }

    if status.is_some_and(|code| code != 0) {
        return format!("failed: {}", reason());
    }

    // A clean run that matched nothing. The two answers a caller has to be able
    // to tell apart are "this video has no captions" and "it has captions, in
    // tags you did not ask for" — so the tags it *does* have are named, and so
    // is the flag that widens the match.
    if read.authored.is_empty() && read.automatic_tags.is_empty() {
        return "this video has no subtitles at all, in any language".to_string();
    }
    // The authored tags in full, the machine ones as a count. YouTube lists
    // several hundred of the latter and they sort ahead of everything useful,
    // so a sample of them is a sample of `aa-de, ab-de, af-de` — noise in the
    // place where the answer should be.
    let mut have = Vec::new();
    if !read.authored.is_empty() {
        have.push(format!("captions in {}", read.authored.join(", ")));
    }
    if !read.automatic_tags.is_empty() {
        have.push(format!(
            "{} automatic caption tag(s)",
            read.automatic_tags.len()
        ));
    }
    format!(
        "no subtitles tagged `{want}`. This video has {}. A machine translation is tagged \
         by its pair, `{want}-en` rather than `{want}` — and `--lang` takes a regex, so \
         `--lang '{want}.*'` finds it.",
        have.join(" and ")
    )
}

/// The reply, in the shape the engine's own `transcript` answers with.
///
/// One shape across both lanes on purpose. An agent that has learned to read a
/// transcript should not have to learn a second reading for the same question
/// asked a different way — what differs is `source` and `evidence`, which is
/// exactly the part that *should* differ and the part it must not miss.
fn render(url: &str, session: &bs::Session, read: &Read, note: &str) -> Value {
    let track = json!({
        "kind": "captions",
        "language": read.language,
        "default": true,
        "src": url,
        "fetched": true,
        "automatic": read.automatic,
        "cues": read.cues,
        "truncated": read.truncated,
    });
    let mut tracks = vec![track];
    for extra in &read.extra {
        tracks.push(json!({
            "kind": "captions",
            "language": extra.tag,
            "default": false,
            "src": url,
            "fetched": true,
            "automatic": false,
            "cues": extra.cues,
            "truncated": extra.truncated,
        }));
    }
    let media = json!({
        "kind": "video",
        "src": url,
        "label": read.title,
        "duration": read.duration,
        "uploader": read.uploader,
        "captions": read.authored,
        "automatic_captions": read.automatic_tags,
        "tracks": tracks,
    });

    json!({
        "ok": true,
        "url": url,
        "empty": read.cues.is_empty(),
        "cues": read.cues.len(),
        "source": NAME,
        // The sentence that keeps the request log's claim true. Said in the
        // reply and not only in the audit, because the reply is what a model
        // reads and the audit is what a person reads afterwards.
        "evidence": evidence(session),
        "media": [media],
        "note": note,
        "text": text(url, session, read, note),
    })
}

/// What saw this lane's traffic, said without rounding up.
///
/// **Branching on the session's lane, not on its placement**, and that
/// distinction is the whole of this function. Being in a box is not the same as
/// being behind a boundary: [`bs::Session::lane_for`] awards `HostObserved`
/// only for enforcement outside the engine, an egress allowlist the box applies
/// or a net mode that lets nothing out, and a boxed session on a tier that
/// confines files and environment but not network is `EngineClaimed`.
///
/// The first version of this claimed the boundary for every boxed session. On
/// this host that claim was already false the moment it was written: the tiers
/// that enforce egress cannot hold a resident browser session on Linux today,
/// so a boxed session is on `workspace` or `process`, and neither has a network
/// boundary at all. `lane_for`'s own comment names this as the one error the
/// product cannot afford, in the direction it cannot afford it in.
fn evidence(session: &bs::Session) -> String {
    let common = "It is not the engine, so none of its fetches are in `h5i browser requests`. \
                  The run itself is in `h5i browser audit`.";
    match (&session.placement, session.lane) {
        (bs::Placement::Box { name }, bs::Lane::HostObserved) => format!(
            "helper-observed. `{NAME}` ran inside box `{name}`, whose boundary enforces egress, \
             so its traffic crossed the same enforcement the engine's did. {common}"
        ),
        // In a box, and the box does not police what leaves it. Said plainly:
        // the containment that *is* there is real and is not a network one, and
        // reporting it as one would be worth less than reporting nothing.
        (bs::Placement::Box { name }, bs::Lane::EngineClaimed) => format!(
            "helper-observed. `{NAME}` ran inside box `{name}`, which confines its files and \
             its environment and **not** its network — that box's tier enforces no egress \
             boundary, so nothing outside `{NAME}` itself saw these fetches. {common}"
        ),
        (bs::Placement::Host, _) => format!(
            "helper-observed. `{NAME}` ran on this machine, outside the engine and outside any \
             boundary h5i enforces: nothing here checked its fetches against this session's \
             policy. {common}"
        ),
    }
}

/// The human reading, fenced like the engine's.
///
/// A transcript fetched by a helper is a stranger's words exactly as one parsed
/// out of a `<track>` is, and it reaches the same reader making the same
/// decision. The fence is not about which program did the fetching.
fn text(url: &str, session: &bs::Session, read: &Read, note: &str) -> String {
    use h5i_browser_light::snapshot::{CONTENT_BEGIN, CONTENT_END};

    let mut out = format!("url: {url}\nsource: {NAME}\nevidence: {}\n", evidence(session));
    if let Some(title) = &read.title {
        out.push_str(&format!("title: {}\n", collapse(title)));
    }
    if let Some(uploader) = &read.uploader {
        out.push_str(&format!("uploader: {}\n", collapse(uploader)));
    }
    if read.automatic {
        out.push_str(
            "note: these are automatic captions, machine-transcribed. Names and technical \
             terms in them are frequently wrong.\n",
        );
    }
    out.push_str(&format!("note: {note}\n"));
    // Authored captions only. YouTube lists a couple of hundred machine
    // translations beside them, and printing that list is how the two real tags
    // become unfindable.
    let others: Vec<&String> = read
        .authored
        .iter()
        .filter(|tag| Some(tag.as_str()) != read.language.as_deref())
        .collect();
    if !others.is_empty() {
        out.push_str(&format!(
            "other captions: {}\n",
            others.iter().map(|t| t.as_str()).collect::<Vec<_>>().join(", ")
        ));
    }
    if !read.automatic_tags.is_empty() {
        // "automatic captions", not "machine translations": the list holds both
        // a video's own machine transcript and YouTube's translations of it,
        // and naming it after the second kind mislabels the first — which is
        // the one a caller actually wants when there are no authored captions.
        out.push_str(&format!(
            "automatic captions available: {} tag(s)\n",
            read.automatic_tags.len()
        ));
    }
    if let Some(truncated) = &read.truncated {
        out.push_str(&format!("note: {truncated}\n"));
    }

    let render_cues = |cues: &[h5i_browser_light::transcript::Cue]| {
        cues.iter()
            .map(|cue| {
                format!(
                    "[{}] {}",
                    h5i_browser_light::transcript::stamp(cue.start),
                    cue.text
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let mut body = render_cues(&read.cues);
    for extra in &read.extra {
        body.push_str(&format!(
            "\n\n## track {}\n",
            extra.tag.as_deref().unwrap_or("(unnamed)")
        ));
        if let Some(truncated) = &extra.truncated {
            body.push_str(&format!("note: {truncated}\n"));
        }
        body.push_str(&render_cues(&extra.cues));
    }

    out.push_str(CONTENT_BEGIN);
    out.push('\n');
    // The same sentence the engine's own readings carry. A caption file fetched
    // by a helper is a stranger's words exactly as one parsed out of a
    // `<track>` is, and it reaches the same reader making the same decision;
    // fencing it with a weaker framing than the other lane would say otherwise.
    out.push_str(h5i_browser_light::snapshot::UNTRUSTED_NOTE);
    out.push_str("\n\n");
    out.push_str(&body.replace(CONTENT_BEGIN, "[fence marker removed]")
        .replace(CONTENT_END, "[fence marker removed]"));
    out.push('\n');
    out.push_str(CONTENT_END);
    out.push('\n');
    out
}

/// Whitespace and control characters out, so nothing from outside spans a line.
///
/// The same rule the engine applies to page text, applied here for the same
/// reason: a helper's stderr and a video's title are both strings someone else
/// wrote, and they are printed to a terminal by the CLI.
fn collapse(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut spaced = false;
    for ch in input.chars() {
        if ch.is_whitespace() {
            spaced = true;
            continue;
        }
        if ch.is_control() {
            continue;
        }
        if spaced && !out.is_empty() {
            out.push(' ');
        }
        spaced = false;
        out.push(ch);
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_http_urls_reach_the_helper() {
        assert!(check_url("https://site.example/watch?v=1").is_ok());
        assert!(check_url("http://site.example/v").is_ok());
        // The one that matters: this lane fetches from the network, and a
        // program handed a `file://` reads the machine instead.
        let refused = check_url("file:///etc/passwd").unwrap_err().to_string();
        assert!(refused.contains("does not read this machine"), "{refused}");
        assert!(check_url("not a url").is_err());
    }

    /// The argv is recorded as a fact about what ran, so the flags that make it
    /// *true* are the ones worth pinning: no config file may add to it, and no
    /// media may be fetched by it.
    #[test]
    fn the_recorded_argv_cannot_be_added_to_by_a_config_file() {
        let argv = build_argv("https://site.example/v", "en.*", "/tmp/x/%(id)s", true);
        assert!(argv.contains(&"--ignore-config".to_string()), "{argv:?}");
        assert!(argv.contains(&"--skip-download".to_string()), "{argv:?}");
        assert!(argv.contains(&"--no-playlist".to_string()), "{argv:?}");
        // Data, not flags. A URL beginning with `-` is the oldest argv bug.
        let end = argv.len();
        assert_eq!(argv[end - 2], "--");
        assert_eq!(argv[end - 1], "https://site.example/v");
    }

    /// yt-dlp is given a pattern, so more than one file legitimately arrives.
    /// Taking the alphabetically first hands back `en-orig` — the machine
    /// transcription — while the author's own `en` captions sit beside it.
    #[test]
    fn the_requested_language_wins_over_whatever_sorted_first() {
        let dir = std::env::temp_dir().join(format!("h5i-helper-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let vtt = "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\n";
        std::fs::write(dir.join("abc.en-orig.vtt"), format!("{vtt}automatic\n")).unwrap();
        std::fs::write(dir.join("abc.en.vtt"), format!("{vtt}authored\n")).unwrap();

        let read = collect(&dir, Some("en"), 4096, false);
        assert_eq!(read.language.as_deref(), Some("en"));
        assert_eq!(read.cues[0].text, "authored");
        assert!(!read.automatic);
        // And both are named, so a caller that wanted the other can see it.
        // Worth noting what this test would be without the preference above:
        // `-` sorts before `.`, so `abc.en-orig.vtt` really is the first file
        // in the directory and the machine transcription would have won.
        assert_eq!(read.cues[0].text, "authored");

        // Asked for the automatic one by name, it is what comes back.
        let read = collect(&dir, Some("en-orig"), 4096, false);
        assert_eq!(read.cues[0].text, "automatic");
        assert!(read.automatic, "and it is labelled as machine-transcribed");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The shape YouTube actually answers `en.*` with, and the bug it caught:
    /// `en` is the author's captions, `en-de` and `en-en` are machine
    /// translations of them, and `-` sorts before `.`, so taking the first file
    /// hands back a translation of a translation.
    #[test]
    fn the_authors_captions_beat_youtubes_translations_of_them() {
        let dir = std::env::temp_dir().join(format!("h5i-yt-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let vtt = "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\n";
        for tag in ["en", "en-de", "en-en"] {
            std::fs::write(dir.join(format!("v.{tag}.vtt")), format!("{vtt}{tag}\n")).unwrap();
        }

        let read = collect(&dir, Some("en"), 4096, false);
        assert_eq!(read.language.as_deref(), Some("en"));
        assert_eq!(read.cues[0].text, "en", "an exact tag match wins over a prefix one");
        assert_eq!(read.language.as_deref(), Some("en"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The live-run bug, pinned. A transcript plus a rate limit on the *next*
    /// language is a complete answer, and reporting it as a failure sends a
    /// caller retrying work it already has.
    #[test]
    fn a_transcript_that_arrived_before_a_later_error_is_still_an_answer() {
        let read = Read {
            language: Some("en".into()),
            cues: vec![h5i_browser_light::transcript::Cue {
                start: 1.0,
                end: 2.0,
                text: "Hello.".into(),
            }],
            ..Read::default()
        };
        let note = describe(&read, "en", Some(1), "ERROR: HTTP Error 429: Too Many Requests\n");
        assert!(note.starts_with("1 cue(s) in en"), "{note}");
        assert!(!note.starts_with("failed"), "{note}");
        // And the partial failure is not hidden: it is what explains the
        // missing title beside it.
        assert!(note.contains("429"), "{note}");
    }

    /// "This video has no captions" and "it has captions, in tags you did not
    /// ask for" are different answers, and the second one has to name both the
    /// tags and the way to widen the match.
    #[test]
    fn a_clean_run_that_matched_nothing_names_what_the_video_does_have() {
        let read = Read {
            authored: vec!["de".into(), "fr".into()],
            automatic_tags: vec!["de".into()],
            ..Read::default()
        };
        let note = describe(&read, "en", Some(0), "");
        assert!(note.contains("no subtitles tagged `en`"), "{note}");
        assert!(note.contains("captions in de, fr"), "{note}");
        assert!(note.contains("1 automatic caption tag"), "{note}");
        assert!(note.contains("--lang 'en.*'"), "the escape hatch is named: {note}");
    }

    /// yt-dlp matches `--sub-langs` with a regex `fullmatch`, so the default
    /// has to be the bare tag. `en.*` against YouTube is three downloads, and
    /// the third is what got rate-limited on the first live run.
    #[test]
    fn the_default_language_is_an_exact_tag_and_not_a_prefix() {
        let argv = build_argv("https://x.test/v", "en", "/tmp/x/%(id)s", true);
        let at = argv.iter().position(|a| a == "--sub-langs").expect("flag");
        assert_eq!(argv[at + 1], "en", "not `en.*`: {argv:?}");
    }

    /// The one that would have hurt somebody else. yt-dlp resolves
    /// `--sub-langs all` against the languages it is willing to *write*, and
    /// `--write-auto-subs` puts every machine translation into that set: 314 on
    /// the first video this lane was pointed at, 940 on the second. Asking for
    /// all of them is several hundred requests to a third party from one flag,
    /// a rate limit within seconds, and nothing anybody wanted.
    ///
    /// So `--all` asks only for the tracks somebody wrote, which is what `--all`
    /// means on the `<track>` lane too.
    #[test]
    fn all_asks_for_authored_tracks_and_not_every_machine_translation() {
        let every = build_argv("https://x.test/v", "all,-live_chat", "/tmp/x/%(id)s", false);
        assert!(every.contains(&"--write-subs".to_string()), "{every:?}");
        assert!(
            !every.contains(&"--write-auto-subs".to_string()),
            "with automatic captions on, `all` is every language YouTube can translate \
             into: {every:?}"
        );

        // A single language still asks for both: a video whose only transcript
        // is machine-generated is exactly what this lane exists to reach, and
        // it is one file either way.
        let one = build_argv("https://x.test/v", "en", "/tmp/x/%(id)s", true);
        assert!(one.contains(&"--write-auto-subs".to_string()), "{one:?}");
    }

    fn session_at(placement: bs::Placement, lane: bs::Lane) -> bs::Session {
        session_with_control(placement, lane, None, None)
    }

    fn session_with_control(
        placement: bs::Placement,
        lane: bs::Lane,
        file: Option<&str>,
        witness: Option<&str>,
    ) -> bs::Session {
        let mut session: bs::Session = serde_json::from_value(serde_json::json!({
            "id": "br_test", "engine": "h5i-light",
            "placement": placement, "lane": lane,
            "url": "https://site.example/", "started_at": "2026-01-01T00:00:00Z",
            "expires_at": null, "storage": "ephemeral", "policy_digest": "sha256:0",
            "restored_from": null, "state": "live", "ended_at": null, "end_reason": null,
            "control": {"channel": "socket", "file": file, "witness": witness, "pid": null},
        }))
        .expect("a session record");
        session.lane = lane;
        session
    }

    /// Being in a box is not the same as being behind a boundary. On Linux
    /// today the tiers that enforce egress cannot hold a resident browser
    /// session at all, so every boxed session is on a tier that confines files
    /// and environment and not network — and the first version of this claimed
    /// the boundary for all of them.
    #[test]
    fn a_box_without_an_egress_boundary_is_not_reported_as_having_one() {
        let unenforced = evidence(&session_at(
            bs::Placement::Box { name: "capbox".into() },
            bs::Lane::EngineClaimed,
        ));
        assert!(unenforced.contains("not** its network"), "{unenforced}");
        assert!(
            !unenforced.contains("crossed the same enforcement"),
            "a process-tier box polices files, not egress: {unenforced}"
        );

        let enforced = evidence(&session_at(
            bs::Placement::Box { name: "capbox".into() },
            bs::Lane::HostObserved,
        ));
        assert!(enforced.contains("enforces egress"), "{enforced}");

        // And neither of them ever claims the request log saw it.
        for text in [unenforced, enforced, evidence(&session_at(bs::Placement::Host, bs::Lane::EngineClaimed))] {
            assert!(text.contains("not the engine"), "{text}");
            assert!(
                text.contains("none of its fetches are in `h5i browser requests`"),
                "{text}"
            );
        }
    }

    /// The two views of one directory, which is the whole of what makes a
    /// boxed run readable: the helper writes at the box's path and h5i reads at
    /// this machine's, and the session record is where both come from rather
    /// than being re-derived here.
    #[test]
    fn a_boxed_run_names_the_directory_in_both_filesystems() {
        let session = session_with_control(
            bs::Placement::Box { name: "wsbox".into() },
            bs::Lane::EngineClaimed,
            Some("/tmp/h5i-browser.sock"),
            Some("/home/u/.h5i/env/wsbox/tmp/h5i-browser.sock"),
        );
        let work = workspace(Path::new("/state"), &session).expect("both views");
        assert_eq!(work.in_helper, "/tmp/helper-br_test/%(id)s", "the box's own path");
        assert_eq!(
            work.host,
            Path::new("/home/u/.h5i/env/wsbox/tmp/helper-br_test"),
            "and this machine's view of the same directory"
        );
    }

    /// Refused, not worked around. The helper would otherwise run, write where
    /// h5i cannot read, and hand back a successful-looking run that produced
    /// nothing.
    #[test]
    fn a_box_whose_tmp_this_machine_cannot_see_is_refused() {
        let session = session_with_control(
            bs::Placement::Box { name: "imagebox".into() },
            bs::Lane::EngineClaimed,
            Some("/tmp/h5i-browser.sock"),
            None,
        );
        let err = workspace(Path::new("/state"), &session)
            .expect_err("no host view, no run")
            .to_string();
        assert!(err.contains("cannot see box `imagebox`'s /tmp"), "{err}");
        // It must not assert a cause it cannot check: an image-backed tier and
        // a session that never came up look identical from here.
        assert!(err.contains("never fully came up"), "{err}");
    }

    #[test]
    fn a_host_run_writes_beside_the_session() {
        let session = session_at(bs::Placement::Host, bs::Lane::EngineClaimed);
        let work = workspace(Path::new("/state"), &session).expect("a host workspace");
        assert!(work.host.ends_with("helper-br_test"), "{:?}", work.host);
        assert!(
            work.in_helper.ends_with("helper-br_test/%(id)s"),
            "{}",
            work.in_helper
        );
        assert_eq!(
            work.in_helper,
            work.host.join("%(id)s").display().to_string(),
            "on the host the two views are one directory"
        );
    }

    /// The lane's refusal to leave the box. A boxed session runs the helper in
    /// its box or not at all: falling back to the host would move the session's
    /// network to a boundary its caller did not choose.
    #[test]
    fn a_boxed_session_carries_the_helper_into_the_box() {
        let session = session_with_control(
            bs::Placement::Box { name: "wsbox".into() },
            bs::Lane::EngineClaimed,
            Some("/tmp/h5i-browser.sock"),
            Some("/tmp/host-view/h5i-browser.sock"),
        );
        // `run` execs, so the argv is checked rather than run: what matters is
        // that the box is named and the helper is inside it.
        assert!(matches!(session.placement, bs::Placement::Box { .. }));
        let argv = build_argv("https://x.test/v", "en", "/tmp/helper/%(id)s", true);
        assert!(argv.last().is_some_and(|a| a == "https://x.test/v"));
    }

    fn workspace_at(dir: &Path) -> Workspace {
        Workspace {
            in_helper: dir.join("%(id)s").display().to_string(),
            host: dir.to_path_buf(),
        }
    }

    /// `h5i box run` merges the child's stderr into its own stdout and keeps
    /// stderr for its receipt line. Reading stderr on the boxed path therefore
    /// yields h5i's own bookkeeping, which `describe` would hand a caller as
    /// the reason yt-dlp failed: it says nothing about the failure, and it
    /// reads as though h5i were the thing that broke.
    #[test]
    fn a_boxed_helpers_complaint_is_its_own_and_not_h5is_receipt_line() {
        let dir = std::env::temp_dir().join(format!("h5i-box-io-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Exactly the shape observed from `h5i box run`.
        std::fs::write(
            dir.join("stdout.log"),
            "[youtube] Extracting URL\n\n----- stderr -----\nERROR: Video unavailable\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("stderr.log"),
            "\u{25c8}  receipt f31bdc823671183e (box env/human/probe1, policy 47084e)              \u{b7} exit 1 \u{b7} wall 3ms\n",
        )
        .unwrap();
        let work = workspace_at(&dir);

        let boxed = complaint(&bs::Placement::Box { name: "probe1".into() }, &work);
        assert!(boxed.contains("Video unavailable"), "{boxed}");
        assert!(!boxed.contains("receipt"), "h5i's own row is not the helper's: {boxed}");

        // The reason a caller actually sees.
        let note = describe(&Read::default(), "en", Some(1), &boxed);
        assert!(note.contains("Video unavailable"), "{note}");
        assert!(!note.contains("wall 3ms"), "{note}");

        // On the host the child's stderr really is the child's stderr.
        let on_host = complaint(&bs::Placement::Host, &work);
        assert!(on_host.contains("receipt"), "the host path reads stderr: {on_host}");

        // And the receipt is not thrown away: it is the box's own record of
        // the run, and the audit row names it.
        let boxed_session = session_at(
            bs::Placement::Box { name: "probe1".into() },
            bs::Lane::EngineClaimed,
        );
        assert_eq!(
            box_receipt(&boxed_session, &work).as_deref(),
            Some("f31bdc823671183e")
        );
        // And never for a run that had no box: on the host that same file is
        // yt-dlp's own stderr, so a line of it mentioning a receipt would put a
        // false evidence claim in the reply.
        let host_session = session_at(bs::Placement::Host, bs::Lane::EngineClaimed);
        assert!(
            box_receipt(&host_session, &work).is_none(),
            "a host run has no box receipt to claim"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A child that wrote nothing to stderr gets no banner, and its *stdout*
    /// must not be promoted into a complaint.
    #[test]
    fn a_quiet_boxed_child_has_no_complaint_rather_than_a_borrowed_one() {
        let dir = std::env::temp_dir().join(format!("h5i-box-quiet-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("stdout.log"), "[info] Downloading subtitles: en\n").unwrap();
        let work = workspace_at(&dir);

        let boxed = complaint(&bs::Placement::Box { name: "probe1".into() }, &work);
        assert!(boxed.is_empty(), "{boxed}");
        let boxed_session = session_at(
            bs::Placement::Box { name: "probe1".into() },
            bs::Lane::EngineClaimed,
        );
        assert!(
            box_receipt(&boxed_session, &work).is_none(),
            "no receipt file, no receipt"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Clamped rather than trusted. Zero would stop every run before it began,
    /// and a very large value would turn the budget into no budget, which is
    /// the thing it exists not to be. Nonsense falls back to the default rather
    /// than to either edge.
    /// A box's `/tmp` is the host's on a workspace-tier box, so a directory
    /// named only `helper` there is shared by every box and every process on
    /// the machine. `collect` reads whatever it finds, so that is a path by
    /// which anyone can hand a model a transcript.
    #[test]
    fn the_workspace_is_named_for_the_session_so_two_boxes_cannot_share_one() {
        let a = session_with_control(
            bs::Placement::Box { name: "one".into() },
            bs::Lane::EngineClaimed,
            Some("/tmp/h5i-browser.sock"),
            Some("/tmp/h5i-browser.sock"),
        );
        let mut b = a.clone();
        b.id = "br_other".into();

        let wa = workspace(Path::new("/state"), &a).unwrap();
        let wb = workspace(Path::new("/state"), &b).unwrap();
        assert_ne!(wa.host, wb.host, "two sessions, two directories");
        assert_ne!(wa.in_helper, wb.in_helper);
        assert!(wa.host.to_string_lossy().contains("br_test"), "{:?}", wa.host);
    }

    /// The directory has to be one this run made. `collect` reads whatever is
    /// in it and reports it as the transcript, so a wipe whose failure is
    /// discarded plus a `create_dir_all` that is happy with an existing
    /// directory is how somebody else's file becomes h5i's answer.
    #[test]
    fn a_workspace_that_could_not_be_cleared_is_refused_rather_than_read() {
        let dir = std::env::temp_dir().join(format!("h5i-scrub-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let work = workspace_at(&dir.join("work"));

        // A clean run makes the directory.
        scrub(&work).expect("first run creates it");
        assert!(work.host.is_dir());

        // And a second run starts from a new one rather than from what is there.
        std::fs::write(work.host.join("planted.en.vtt"), "WEBVTT\n").unwrap();
        scrub(&work).expect("second run clears it");
        assert!(
            std::fs::read_dir(&work.host).unwrap().next().is_none(),
            "the planted file survived into a run that would have read it"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `--lang` is a regex to yt-dlp and a literal here, and the escape hatch
    /// this lane prints by name is `--lang 'en.*'`. Without the literal head,
    /// no tag equals or starts with `en.*`, selection falls through to whatever
    /// sorted first, and `-` sorts before `.` — so following the tool's own
    /// advice returned `en-de`, a translation of a translation.
    #[test]
    fn a_regex_language_still_chooses_the_right_file() {
        assert_eq!(literal_prefix("en"), "en");
        assert_eq!(literal_prefix("en.*"), "en");
        assert_eq!(literal_prefix("ja.*"), "ja");
        assert_eq!(literal_prefix("(en|de)"), "(en|de)", "no literal head to use");
        assert_eq!(literal_prefix(""), "");

        let dir = std::env::temp_dir().join(format!("h5i-regex-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let vtt = "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\n";
        std::fs::write(dir.join("v.en.vtt"), format!("{vtt}authored\n")).unwrap();
        std::fs::write(dir.join("v.en-de.vtt"), format!("{vtt}translated\n")).unwrap();

        let read = collect(&dir, Some(literal_prefix("en.*")), 4096, false);
        assert_eq!(read.language.as_deref(), Some("en"));
        assert_eq!(read.cues[0].text, "authored");
        // A pattern brings back several files and the caller asked for one
        // language, so the translation beside it is not rendered too.
        assert!(read.extra.is_empty(), "a --lang pattern is not --all");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A video with automatic captions and no authored ones gets them written
    /// as plain `<id>.en.vtt`, which the filename cannot distinguish from an
    /// author's track — so the "machine-transcribed" warning went missing on
    /// exactly the transcript that needed it.
    #[test]
    fn an_automatic_only_track_is_labelled_from_the_info_file() {
        let dir = std::env::temp_dir().join(format!("h5i-autolabel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("v.en.vtt"),
            "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nMachine heard this.\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("v.info.json"),
            r#"{"subtitles":{},"automatic_captions":{"en":[]}}"#,
        )
        .unwrap();

        let read = collect(&dir, Some("en"), 4096, false);
        assert!(read.automatic, "the tag is only in automatic_captions");
        assert!(describe(&read, "en", Some(0), "").contains("automatic captions"));

        // The same filename, where the video *does* have authored captions in
        // that language, is the author's track and is not labelled.
        std::fs::write(
            dir.join("v.info.json"),
            r#"{"subtitles":{"en":[]},"automatic_captions":{"en":[]}}"#,
        )
        .unwrap();
        let read = collect(&dir, Some("en"), 4096, false);
        assert!(!read.automatic, "yt-dlp prefers the authored track and so do we");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `--all` downloads every authored track and used to report one, so the
    /// extra requests bought nothing a caller could not already read off the
    /// tag list. The CLI help says "read every text track".
    #[test]
    fn every_downloaded_track_is_reported_and_not_just_the_chosen_one() {
        let dir = std::env::temp_dir().join(format!("h5i-allrep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let vtt = "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\n";
        for tag in ["en", "de", "fr"] {
            std::fs::write(dir.join(format!("v.{tag}.vtt")), format!("{vtt}said in {tag}\n"))
                .unwrap();
        }

        // Without `--all` the caller asked for one language and gets one.
        assert!(collect(&dir, Some("en"), 4096, false).extra.is_empty());

        let read = collect(&dir, Some("en"), 4096, true);
        assert_eq!(read.language.as_deref(), Some("en"));
        assert_eq!(read.cues[0].text, "said in en");
        let extra: Vec<&str> = read
            .extra
            .iter()
            .filter_map(|e| e.tag.as_deref())
            .collect();
        assert_eq!(extra, vec!["de", "fr"], "the other two are carried too");
        assert_eq!(read.extra[0].cues[0].text, "said in de");

        // One file is the ordinary path and has no extras to carry.
        let _ = std::fs::remove_file(dir.join("v.de.vtt"));
        let _ = std::fs::remove_file(dir.join("v.fr.vtt"));
        assert!(collect(&dir, Some("en"), 4096, true).extra.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_budget_override_is_clamped_and_nonsense_keeps_the_default() {
        assert_eq!(budget_from(None), BUDGET);
        assert_eq!(budget_from(Some("not a number")), BUDGET);
        assert_eq!(budget_from(Some("")), BUDGET);
        assert_eq!(budget_from(Some(" 30 ")), Duration::from_secs(30));
        assert_eq!(budget_from(Some("0")), Duration::from_secs(1), "never zero");
        assert_eq!(
            budget_from(Some("999999")),
            Duration::from_secs(1800),
            "a budget that never expires is not a budget"
        );
    }

    /// The stop is a *result*, not an error. A run killed at the budget has
    /// usually written some of what it was asked for, and this used to return
    /// an error and throw all of it away.
    #[test]
    fn a_run_stopped_at_the_budget_keeps_what_it_had() {
        let read = Read {
            language: Some("en".into()),
            cues: vec![h5i_browser_light::transcript::Cue {
                start: 1.0,
                end: 4.0,
                text: "The first ten minutes.".into(),
            }],
            ..Read::default()
        };
        let stopped = "still working\nh5i stopped `yt-dlp` after 3s, its whole budget for one \
                       transcript.";
        let note = describe(&read, "en", Some(-1), stopped);
        assert!(note.starts_with("1 cue(s) in en"), "{note}");
        assert!(note.contains("stopped"), "the stop is named, not hidden: {note}");
        assert!(!note.starts_with("failed"), "{note}");

        // And with nothing to show for it, the same stop *is* a failure.
        let empty = describe(&Read::default(), "en", Some(-1), stopped);
        assert!(empty.starts_with("failed:"), "{empty}");
    }

    /// A flag that asks for more must never hand back less. `--all` drops the
    /// automatic flag so it does not ask YouTube for three hundred languages,
    /// and on a video whose only transcript is machine-generated that made
    /// `--all` return nothing where passing no flag returned the transcript.
    /// The fallback picks exactly one automatic track.
    #[test]
    fn the_all_fallback_picks_one_track_in_the_order_a_caller_expects() {
        // The language they named wins.
        assert_eq!(fallback_tag(&["de".into(), "en".into()], "en").as_deref(), Some("en"));
        // Then anything beginning with it.
        assert_eq!(fallback_tag(&["en-GB".into(), "de".into()], "en").as_deref(), Some("en-GB"));
        // Then a source transcript over a translation of one: `de-en` is
        // German rendered from English, `de` is what was actually said.
        assert_eq!(
            fallback_tag(&["fr-en".into(), "de".into()], "en").as_deref(),
            Some("de"),
            "a bare tag is a source track"
        );
        // Then whatever there is.
        assert_eq!(fallback_tag(&["fr-en".into()], "en").as_deref(), Some("fr-en"));
        // And nothing to fall back to is not a fallback.
        assert_eq!(fallback_tag(&[], "en"), None);
    }

    #[test]
    fn the_available_tags_come_off_the_info_file() {
        let info = serde_json::json!({
            "subtitles": {"en": [], "de": []},
            "automatic_captions": {"fr": [], "en": []},
        });
        assert_eq!(tags(&info["subtitles"]), vec!["de", "en"]);
        assert_eq!(tags(&info["automatic_captions"]), vec!["en", "fr"]);
        assert!(tags(&serde_json::Value::Null).is_empty());
    }

    #[test]
    fn the_language_tag_comes_off_the_filename() {
        assert_eq!(tag_of(Path::new("/t/abc123.en.vtt")).as_deref(), Some("en"));
        assert_eq!(
            tag_of(Path::new("/t/abc123.en-orig.vtt")).as_deref(),
            Some("en-orig")
        );
        assert_eq!(tag_of(Path::new("/t/noextension")), None);
    }

    /// A failure is a result and is described as one. The alternative — an
    /// empty transcript with no reason — reads as a video that said nothing.
    #[test]
    fn a_failed_run_reports_the_helpers_own_last_word() {
        let read = Read::default();
        let note = describe(&read, "en", Some(1), "ERROR: Video unavailable\n");
        assert!(note.starts_with("failed:"), "{note}");
        assert!(note.contains("Video unavailable"), "{note}");
    }

    #[test]
    fn a_clean_run_with_no_subtitles_is_not_a_failure() {
        let note = describe(&Read::default(), "en", Some(0), "");
        assert!(note.contains("no subtitles at all"), "{note}");
        assert!(!note.contains("failed"), "{note}");
    }

    /// Automatic captions mishear names, and a reader quoting one should know
    /// that before it quotes.
    #[test]
    fn automatic_captions_are_named_as_automatic() {
        let read = Read {
            language: Some("en-orig".into()),
            automatic: true,
            cues: vec![h5i_browser_light::transcript::Cue {
                start: 1.0,
                end: 2.0,
                text: "Hello.".into(),
            }],
            ..Read::default()
        };
        let note = describe(&read, "en", Some(0), "");
        assert!(note.contains("automatic captions"), "{note}");
    }

    /// The helper's own output is a stranger's bytes reaching a model, exactly
    /// as a page's text is.
    #[test]
    fn a_helpers_stderr_cannot_span_a_line() {
        let note = describe(&Read::default(), "en", Some(1), "ERROR: one\u{1b}[2Jtwo\nlast line\n");
        assert!(!note.contains('\n'), "{note}");
        assert!(!note.contains('\u{1b}'), "{note}");
    }
}
