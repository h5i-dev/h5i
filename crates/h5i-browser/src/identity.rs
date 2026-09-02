//! A session-wide browser identity shared by HTTP, JavaScript, display, and
//! time-zone reporting.
//!
//! Identity is resolved once and recorded in receipts. Compatible identities
//! are rejected unless the engine supports every capability they require;
//! network location and language/time-zone consistency are outside this module.

use std::collections::BTreeSet;
use std::path::Path;

use h5i_error::H5iError;
use serde::{Deserialize, Serialize};

/// Which of the two fingerprinting strategies an identity is pursuing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Answer as h5i, and answer truthfully.
    Native,
    /// Answer as h5i, with what varies between installations pinned.
    Privacy,
    /// Answer as another browser, as far as this engine can back the claim.
    Compatible,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Native => "native",
            Mode::Privacy => "privacy",
            Mode::Compatible => "compatible",
        }
    }
}

/// The engine an identity claims to be.
///
/// Not cosmetic: it decides which tokens the agent string must carry, and
/// which it must *not*. Safari sends no `Sec-CH-UA` at all and Chrome always
/// does, so a family that got this wrong would be detectable from one header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Family {
    /// This engine, under its own name.
    H5i,
    Firefox,
    Chrome,
    Safari,
}

impl Family {
    pub fn as_str(self) -> &'static str {
        match self {
            Family::H5i => "h5i",
            Family::Firefox => "firefox",
            Family::Chrome => "chrome",
            Family::Safari => "safari",
        }
    }

    /// The product token the agent string must carry, before its version.
    fn product_token(self) -> &'static str {
        match self {
            Family::H5i => "h5i-browser",
            Family::Firefox => "Firefox",
            Family::Chrome => "Chrome",
            // Safari's version lives in `Version/`; `Safari/` carries a WebKit
            // build number that has nothing to do with the release.
            Family::Safari => "Version",
        }
    }
}

/// The operating system an identity claims to run on.
///
/// [`Os::Undeclared`] is not "unknown". It is the honest answer for an engine
/// that is not pretending to be a desktop browser on any particular system, and
/// it is what `native` and `privacy` use. It is only valid for [`Family::H5i`]:
/// a browser claiming to be Firefox has an operating system, and refusing to
/// name it would be the incoherence rather than the modesty.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Os {
    Undeclared,
    Windows,
    #[serde(rename = "macos")]
    MacOs,
    Linux,
    Android,
    #[serde(rename = "ios")]
    IOs,
}

impl Os {
    pub fn as_str(self) -> &'static str {
        match self {
            Os::Undeclared => "undeclared",
            Os::Windows => "windows",
            Os::MacOs => "macos",
            Os::Linux => "linux",
            Os::Android => "android",
            Os::IOs => "ios",
        }
    }

    /// What `navigator.platform` must say for this system.
    ///
    /// Fixed strings rather than a range, because these *are* fixed: every
    /// 64-bit Chrome and Firefox on Windows reports `Win32`, and a browser
    /// that reported `Win64` would be the only one in the world doing so.
    fn platforms(self) -> &'static [&'static str] {
        match self {
            Os::Undeclared => &[""],
            Os::Windows => &["Win32"],
            Os::MacOs => &["MacIntel"],
            Os::Linux => &["Linux x86_64", "Linux i686", "Linux aarch64"],
            Os::Android => &["Linux armv8l", "Linux aarch64", "Linux armv7l"],
            Os::IOs => &["iPhone", "iPad"],
        }
    }

    /// A substring the agent string must contain to be claiming this system.
    fn agent_marks(self) -> &'static [&'static str] {
        match self {
            Os::Undeclared => &[],
            Os::Windows => &["Windows NT"],
            Os::MacOs => &["Macintosh"],
            Os::Linux => &["Linux"],
            Os::Android => &["Android"],
            Os::IOs => &["iPhone", "iPad"],
        }
    }

    /// Whether a browser on this system is a phone or tablet browser.
    ///
    /// `None` where it is genuinely a choice. A Linux browser may be either,
    /// and Android's own tablet build sets `mobile` false.
    fn implies_mobile(self) -> Option<bool> {
        match self {
            Os::Windows | Os::MacOs => Some(false),
            Os::IOs => Some(true),
            Os::Undeclared | Os::Linux | Os::Android => None,
        }
    }
}

/// Something an identity needs the engine to actually have.
///
/// Declared by the identity, checked against [`crate::Capabilities`], and the
/// reason a `compatible` identity can be refused before a single byte moves.
/// Every name is spelled out rather than derived: `rename_all` produced
/// `web-gl2`, `java-script` and `web-sockets`, which are not what
/// [`Requirement::as_str`] prints, so a file copied from the output it was given
/// would not parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Requirement {
    /// `navigator.userAgentData`, `getHighEntropyValues`, and the `Sec-CH-UA`
    /// header family that has to agree with them.
    #[serde(rename = "ua-client-hints")]
    UaClientHints,
    #[serde(rename = "webgl2")]
    WebGl2,
    #[serde(rename = "webrtc")]
    WebRtc,
    #[serde(rename = "audio-context")]
    AudioContext,
    #[serde(rename = "media-devices")]
    MediaDevices,
    #[serde(rename = "service-worker")]
    ServiceWorker,
    #[serde(rename = "video")]
    Video,
    #[serde(rename = "canvas-2d")]
    Canvas2d,
    #[serde(rename = "websockets")]
    WebSockets,
    /// The identity's values are only observable to a page that runs script.
    #[serde(rename = "javascript")]
    JavaScript,
}

impl Requirement {
    pub fn as_str(self) -> &'static str {
        match self {
            Requirement::UaClientHints => "ua-client-hints",
            Requirement::WebGl2 => "webgl2",
            Requirement::WebRtc => "webrtc",
            Requirement::AudioContext => "audio-context",
            Requirement::MediaDevices => "media-devices",
            Requirement::ServiceWorker => "service-worker",
            Requirement::Video => "video",
            Requirement::Canvas2d => "canvas-2d",
            Requirement::WebSockets => "websockets",
            Requirement::JavaScript => "javascript",
        }
    }

    /// Whether the engine described by `caps` can back this.
    ///
    /// The arms with no capability field behind them answer `false` rather than
    /// being omitted: this engine has no WebRTC, no `AudioContext`, no
    /// `mediaDevices`, no service workers and no UA client hints, and a table
    /// that quietly skipped them would let an identity requiring one through.
    pub fn met_by(self, caps: &crate::Capabilities) -> bool {
        match self {
            Requirement::WebGl2 => caps.webgl,
            Requirement::Video => caps.video,
            Requirement::Canvas2d => caps.canvas_2d,
            Requirement::WebSockets => caps.websockets,
            Requirement::JavaScript => caps.javascript,
            Requirement::UaClientHints
            | Requirement::WebRtc
            | Requirement::AudioContext
            | Requirement::MediaDevices
            | Requirement::ServiceWorker => false,
        }
    }

    /// Why this engine cannot meet it, for the refusal message.
    pub fn why_unmet(self) -> &'static str {
        match self {
            Requirement::UaClientHints => {
                "this engine sends no Sec-CH-UA and exposes no navigator.userAgentData"
            }
            Requirement::WebGl2 => "this engine has no WebGL",
            Requirement::WebRtc => "this engine has no WebRTC",
            Requirement::AudioContext => "this engine has no Web Audio",
            Requirement::MediaDevices => "this engine has no navigator.mediaDevices",
            Requirement::ServiceWorker => "this engine has no service workers",
            Requirement::Video => "this engine decodes no video",
            Requirement::Canvas2d => "canvas 2D needs --script, and it is off",
            Requirement::WebSockets => "WebSockets need --script, and it is off",
            Requirement::JavaScript => "--script is off, so no page can read these values",
        }
    }
}

/// A time zone this engine can answer for, coherently, in every place a page can
/// ask.
///
/// Fixed-offset zones only, and that is a refusal rather than a shortcut. A page
/// reads local time two ways, `Date.prototype.getTimezoneOffset` and
/// `Intl.DateTimeFormat().resolvedOptions().timeZone`, and a browser whose two
/// answers disagree is caught by the first fingerprinting script that checks.
/// This engine has no time zone database, so for a zone that observes daylight
/// saving it could only pin one offset and be wrong for half the year.
///
/// (`Intl` is absent from this engine entirely, so there is no second answer to
/// contradict `Date`. That is why the fixed-offset half works at all.)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeZone {
    /// The IANA name, as `Intl` would report it if this engine had one.
    pub name: String,
    /// Minutes east of UTC. Constant all year, by construction.
    pub offset_minutes: i32,
}

/// Every zone this engine can back, with the offset it holds all year.
///
/// Each one is here because it does not observe daylight saving. Japan and
/// Korea never have, India and the Gulf never have, and Brazil stopped in 2019.
/// A zone that is missing is missing because adding it would mean shipping a
/// rule that changes twice a year.
const FIXED_OFFSET_ZONES: &[(&str, i32)] = &[
    ("UTC", 0),
    ("Africa/Lagos", 60),
    ("Africa/Johannesburg", 120),
    ("Africa/Nairobi", 180),
    ("America/Bogota", -300),
    ("America/Lima", -300),
    ("America/Phoenix", -420),
    ("America/Sao_Paulo", -180),
    ("Asia/Bangkok", 420),
    ("Asia/Dubai", 240),
    ("Asia/Hong_Kong", 480),
    ("Asia/Jakarta", 420),
    ("Asia/Karachi", 300),
    ("Asia/Kolkata", 330),
    ("Asia/Manila", 480),
    ("Asia/Riyadh", 180),
    ("Asia/Seoul", 540),
    ("Asia/Shanghai", 480),
    ("Asia/Singapore", 480),
    ("Asia/Taipei", 480),
    ("Asia/Tokyo", 540),
    ("Australia/Brisbane", 600),
    ("Pacific/Honolulu", -600),
];

impl TimeZone {
    /// The zone by IANA name, if this engine can hold it all year.
    pub fn named(name: &str) -> Option<Self> {
        FIXED_OFFSET_ZONES
            .iter()
            .find(|(zone, _)| *zone == name)
            .map(|(zone, offset)| Self {
                name: (*zone).to_string(),
                offset_minutes: *offset,
            })
    }

    pub fn utc() -> Self {
        Self {
            name: "UTC".to_string(),
            offset_minutes: 0,
        }
    }

    /// What `HostHooks::local_timezone_offset_seconds` must return.
    pub fn offset_seconds(&self) -> i32 {
        self.offset_minutes * 60
    }

    /// Every zone that can be named, for the error message and for `--help`.
    pub fn available() -> impl Iterator<Item = &'static str> {
        FIXED_OFFSET_ZONES.iter().map(|(zone, _)| *zone)
    }
}

/// The browser an identity claims to be.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Browser {
    pub family: Family,
    /// The release, as the agent string spells it. Checked against the string
    /// rather than trusted beside it.
    pub version: String,
    /// The whole `User-Agent`, sent on the wire and reported by
    /// `navigator.userAgent`. One string, so the two cannot drift.
    pub user_agent: String,
    /// `navigator.vendor`. Empty for Firefox and for h5i; Chrome and Safari
    /// both report `Google Inc.` and `Apple Computer, Inc.` respectively.
    #[serde(default)]
    pub vendor: String,
    /// `navigator.productSub`. `20030107` on Chrome and Safari, `20100101` on
    /// Firefox, and it is checked because a Chrome reporting Firefox's is a
    /// one-property giveaway.
    #[serde(default = "default_product_sub")]
    pub product_sub: String,
}

fn default_product_sub() -> String {
    "20030107".to_string()
}

/// The machine an identity claims to run on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    pub os: Os,
    /// `navigator.platform`.
    #[serde(default)]
    pub platform: String,
    /// `navigator.oscpu`. Firefox alone reports it; everyone else omits it.
    #[serde(default)]
    pub oscpu: String,
    /// Whether this is a phone or tablet browser. Cross-checked against both
    /// the operating system and `max_touch_points`.
    #[serde(default)]
    pub mobile: bool,
    /// `navigator.maxTouchPoints`.
    #[serde(default)]
    pub max_touch_points: u32,
    /// `navigator.hardwareConcurrency`.
    pub hardware_concurrency: u32,
}

/// What an identity says about language.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Locale {
    /// `navigator.languages`, and the `Accept-Language` built from it. First
    /// entry is `navigator.language`.
    pub languages: Vec<String>,
    /// The zone `Date` computes local time from.
    ///
    /// `None` means undeclared, and undeclared means the host's, which is what
    /// `native` does, and which is exactly the value `privacy` exists to pin.
    #[serde(default)]
    pub timezone: Option<TimeZone>,
}

/// How many languages one identity may declare.
///
/// A header cap, and therefore a *coherence* cap. `Accept-Language` carries a
/// descending q-value ladder and browsers stop at ten; carrying more would mean
/// either a q below 0.1 or a header no browser sends. The alternative, discovered
/// by testing it, is that an identity declaring twelve languages was admitted,
/// sent ten on the wire, and reported twelve from `navigator.languages`: the
/// exact disagreement this module exists to prevent, produced by its own
/// accessor.
pub const MAX_LANGUAGES: usize = 10;

impl Locale {
    /// The `Accept-Language` header these languages ask for.
    ///
    /// Built rather than stored beside them, because a header that disagreed with
    /// `navigator.languages` is the single cheapest cross-layer check a server can
    /// run: it sees the header, its script sees the array.
    ///
    /// The q-value ladder is the shape browsers actually send, and it stops at 0.1
    /// rather than going negative.
    pub fn accept_language(&self) -> String {
        self.languages
            .iter()
            // Unreachable for an admitted identity, `incoherences` refuses a
            // longer list, and kept so this can never silently disagree with
            // `navigator.languages` if that check is ever loosened.
            .take(MAX_LANGUAGES)
            .enumerate()
            .map(|(index, tag)| {
                if index == 0 {
                    tag.clone()
                } else {
                    format!("{tag};q={:.1}", 1.0 - (index as f32) * 0.1)
                }
            })
            .collect::<Vec<_>>()
            .join(",")
    }
}

/// The display an identity claims, when it claims one.
///
/// Absent for `native` and `privacy` on purpose. `prelude.js` already refuses to
/// expose `screen`, on the rule that a name which exists and answers wrongly is
/// worse than one that is absent, and a headless engine's honest screen size is
/// a guess. A declared identity is the case that rule was waiting for: the
/// answer is no longer guessed, it is stated, so the object appears.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Screen {
    pub width: u32,
    pub height: u32,
    /// `screen.availWidth`: the display less any system chrome.
    pub avail_width: u32,
    pub avail_height: u32,
    /// `screen.colorDepth` and `screen.pixelDepth`, which are the same number
    /// on every browser that ships.
    pub color_depth: u32,
    /// `window.devicePixelRatio`, in hundredths so the identity round-trips
    /// through TOML and its digest without a float's spelling changing it.
    pub device_pixel_ratio_x100: u32,
}

impl Screen {
    pub fn device_pixel_ratio(&self) -> f32 {
        self.device_pixel_ratio_x100 as f32 / 100.0
    }
}

/// One coherent browser identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub name: String,
    pub mode: Mode,
    pub browser: Browser,
    pub device: Device,
    pub locale: Locale,
    #[serde(default)]
    pub screen: Option<Screen>,
    /// What this identity needs the engine to have before it may be used.
    #[serde(default)]
    pub requires: BTreeSet<Requirement>,
}

/// One way an identity contradicts itself.
///
/// Carried as a list rather than a first failure: someone writing an identity
/// file wants every contradiction at once, not one per run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Incoherence {
    /// The field that is wrong, dotted, as it is spelled in the file.
    pub field: String,
    pub says: String,
}

impl std::fmt::Display for Incoherence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.field, self.says)
    }
}

fn wrong(field: &str, says: impl Into<String>) -> Incoherence {
    Incoherence {
        field: field.to_string(),
        says: says.into(),
    }
}

impl Identity {
    /// What this identity actually reaches, and what it does not.
    ///
    /// Printed by `h5i browser identity check`, and the reason a `compatible`
    /// identity is not called anti-detection. Everything in the second list is
    /// a layer a page or a server can read that this identity has no say over,
    /// so a determined check will still see this engine for what it is. Saying
    /// so is the point: the claim being made is coherence, not invisibility.
    pub const COVERS: &'static [&'static str] = &[
        "User-Agent, on the wire and in navigator.userAgent",
        "Accept-Language, and navigator.languages",
        "navigator.platform, vendor, productSub, oscpu",
        "navigator.hardwareConcurrency and maxTouchPoints",
        "screen geometry and devicePixelRatio, when declared",
        "the offset Date computes local time from, when a zone is declared",
    ];

    pub const DOES_NOT_COVER: &'static [&'static str] = &[
        "the TLS ClientHello, its extension order and its GREASE",
        "the HTTP/2 SETTINGS and pseudo-header order",
        "canvas and WebGL readback, which this engine cannot produce",
        "AudioContext fingerprints, which this engine has none of",
        "the installed font set, which is the host's",
        "pointer and keyboard event timing",
    ];

    /// Every contradiction in this identity, or an empty list.
    pub fn incoherences(&self) -> Vec<Incoherence> {
        let mut found = Vec::new();
        self.check_agent(&mut found);
        self.check_mode(&mut found);
        self.check_device(&mut found);
        self.check_locale(&mut found);
        self.check_screen(&mut found);
        found
    }

    /// The agent string, against everything else that describes the same browser.
    fn check_agent(&self, found: &mut Vec<Incoherence>) {
        let agent = &self.browser.user_agent;
        if agent.trim().is_empty() {
            found.push(wrong("browser.user_agent", "is empty"));
            return;
        }
        // A header value with a control character in it is not a fingerprinting
        // problem, it is a request smuggling one: `reqwest` would refuse the
        // header and the fetch would fail at a layer with nothing useful to say.
        if agent
            .chars()
            .any(|c| c.is_control() || !c.is_ascii() || c == '\u{7f}')
        {
            found.push(wrong(
                "browser.user_agent",
                "carries a control or non-ASCII character, which cannot go in a header",
            ));
        }
        if self.browser.version.trim().is_empty() {
            found.push(wrong("browser.version", "is empty"));
        }

        let token = self.browser.family.product_token();
        let stamp = format!("{token}/{}", self.browser.version);
        if !agent.contains(&stamp) {
            found.push(wrong(
                "browser.user_agent",
                format!(
                    "claims {} {} but the agent string does not carry `{stamp}`",
                    self.browser.family.as_str(),
                    self.browser.version
                ),
            ));
        }
        // Chrome's agent string carries `Safari/537.36` for historical reasons,
        // so "contains Safari" cannot mean Safari. The reverse is decisive:
        // real Safari never carries `Chrome/`, and real Firefox never does
        // either, so either one that does is claiming two browsers at once.
        if matches!(self.browser.family, Family::Firefox | Family::Safari)
            && agent.contains("Chrome/")
        {
            found.push(wrong(
                "browser.user_agent",
                format!(
                    "claims {} but carries Chrome's product token",
                    self.browser.family.as_str()
                ),
            ));
        }

        let marks = self.device.os.agent_marks();
        if !marks.is_empty() && !marks.iter().any(|mark| agent.contains(mark)) {
            found.push(wrong(
                "browser.user_agent",
                format!(
                    "claims {} but the agent string names none of {}",
                    self.device.os.as_str(),
                    marks.join(", ")
                ),
            ));
        }
        // Linux and Android share the `Linux` token, so a Linux claim has to
        // rule Android out explicitly or every Android string would satisfy it.
        if self.device.os == Os::Linux && agent.contains("Android") {
            found.push(wrong(
                "browser.user_agent",
                "claims linux but the agent string says Android",
            ));
        }

        let sub = &self.browser.product_sub;
        let expected = match self.browser.family {
            Family::Firefox => "20100101",
            Family::Chrome | Family::Safari => "20030107",
            // h5i is not claiming anyone else's, so anything it declares is its
            // own to declare.
            Family::H5i => sub.as_str(),
        };
        if sub != expected {
            found.push(wrong(
                "browser.product_sub",
                format!(
                    "is `{sub}`, but every {} reports `{expected}`",
                    self.browser.family.as_str()
                ),
            ));
        }
    }

    /// The mode, against what the identity is claiming to be.
    fn check_mode(&self, found: &mut Vec<Incoherence>) {
        match self.mode {
            // Honesty and impersonation are the two things a mode can mean, and
            // an identity cannot be doing both. `native` and `privacy` are
            // still h5i; that is what makes `privacy` a *reduction* of what h5i
            // discloses rather than a disguise with a different name.
            Mode::Native | Mode::Privacy if self.browser.family != Family::H5i => {
                found.push(wrong(
                    "mode",
                    format!(
                        "is {} but the identity claims to be {}; only `compatible` may name another browser",
                        self.mode.as_str(),
                        self.browser.family.as_str()
                    ),
                ));
            }
            Mode::Compatible if self.browser.family == Family::H5i => {
                found.push(wrong(
                    "mode",
                    "is compatible but the identity claims to be h5i, which is what `native` is for",
                ));
            }
            _ => {}
        }
        if self.mode == Mode::Compatible && self.device.os == Os::Undeclared {
            found.push(wrong(
                "device.os",
                "is undeclared, which no real browser is; a compatible identity names its system",
            ));
        }
        if self.device.os == Os::Undeclared && self.browser.family != Family::H5i {
            found.push(wrong(
                "device.os",
                "is undeclared, which is only honest for h5i's own identity",
            ));
        }
    }

    fn check_device(&self, found: &mut Vec<Incoherence>) {
        let platforms = self.device.os.platforms();
        if !platforms.contains(&self.device.platform.as_str()) {
            found.push(wrong(
                "device.platform",
                format!(
                    "is `{}`, which no {} browser reports; expected one of {}",
                    self.device.platform,
                    self.device.os.as_str(),
                    platforms
                        .iter()
                        .map(|p| if p.is_empty() {
                            "the empty string".to_string()
                        } else {
                            format!("`{p}`")
                        })
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ));
        }
        if let Some(implied) = self.device.os.implies_mobile()
            && implied != self.device.mobile
        {
            found.push(wrong(
                "device.mobile",
                format!(
                    "is {} but {} browsers are always {}",
                    self.device.mobile,
                    self.device.os.as_str(),
                    if implied { "mobile" } else { "not mobile" }
                ),
            ));
        }
        // One-directional on purpose. A phone with no touch points does not
        // exist; a desktop *with* them is every touchscreen laptop, so the
        // reverse is not an error.
        if self.device.mobile && self.device.max_touch_points == 0 {
            found.push(wrong(
                "device.max_touch_points",
                "is 0 on a mobile identity, and no touch device reports that",
            ));
        }
        // `oscpu` is Firefox's alone. A Chrome reporting one is reporting a
        // property Chrome does not have, which is louder than reporting nothing.
        if !self.device.oscpu.is_empty() && self.browser.family != Family::Firefox {
            found.push(wrong(
                "device.oscpu",
                format!(
                    "is set, but only Firefox reports navigator.oscpu and this claims {}",
                    self.browser.family.as_str()
                ),
            ));
        }
        match self.device.hardware_concurrency {
            0 => found.push(wrong(
                "device.hardware_concurrency",
                "is 0, and no browser reports fewer than 1 core",
            )),
            // Chrome caps its own report at 128 and Firefox lower still. A
            // larger number is not a bigger machine, it is an outlier.
            n if n > 128 => found.push(wrong(
                "device.hardware_concurrency",
                format!("is {n}; browsers cap this report at 128"),
            )),
            _ => {}
        }
    }

    fn check_locale(&self, found: &mut Vec<Incoherence>) {
        if self.locale.languages.is_empty() {
            found.push(wrong(
                "locale.languages",
                "is empty, and navigator.languages never is",
            ));
        }
        if self.locale.languages.len() > MAX_LANGUAGES {
            found.push(wrong(
                "locale.languages",
                format!(
                    "declares {} languages; `Accept-Language` carries at most {MAX_LANGUAGES}, \
                     so the wire would offer {MAX_LANGUAGES} while navigator.languages reported \
                     {} — two answers to one question",
                    self.locale.languages.len(),
                    self.locale.languages.len()
                ),
            ));
        }
        for tag in &self.locale.languages {
            let shaped = !tag.is_empty()
                && tag
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-')
                && tag.split('-').all(|part| !part.is_empty());
            if !shaped {
                found.push(wrong(
                    "locale.languages",
                    format!("carries `{tag}`, which is not a language tag"),
                ));
            }
        }
        // A zone this engine cannot hold all year is refused here rather than
        // approximated at the hook: see [`TimeZone`].
        if let Some(zone) = &self.locale.timezone
            && TimeZone::named(&zone.name).as_ref() != Some(zone)
        {
            found.push(wrong(
                "locale.timezone",
                format!(
                    "names `{}`, which this engine cannot answer for consistently; it has no time zone database, so only zones that never observe daylight saving can be declared: {}",
                    zone.name,
                    TimeZone::available().collect::<Vec<_>>().join(", ")
                ),
            ));
        }
    }

    fn check_screen(&self, found: &mut Vec<Incoherence>) {
        let Some(screen) = &self.screen else {
            return;
        };
        if screen.width == 0 || screen.height == 0 {
            found.push(wrong("screen", "has a zero dimension"));
        }
        if screen.avail_width > screen.width || screen.avail_height > screen.height {
            found.push(wrong(
                "screen.avail_width",
                format!(
                    "reports {}x{} available on a {}x{} display, and the workspace is never larger than the screen",
                    screen.avail_width, screen.avail_height, screen.width, screen.height
                ),
            ));
        }
        if !matches!(screen.color_depth, 24 | 30 | 32) {
            found.push(wrong(
                "screen.color_depth",
                format!(
                    "is {}; browsers report 24, 30 or 32",
                    screen.color_depth
                ),
            ));
        }
        if !(50..=400).contains(&screen.device_pixel_ratio_x100) {
            found.push(wrong(
                "screen.device_pixel_ratio_x100",
                format!(
                    "is {} (a ratio of {:.2}), which is outside every shipping display",
                    screen.device_pixel_ratio_x100,
                    screen.device_pixel_ratio()
                ),
            ));
        }
    }

    /// The viewport this identity can be rendered at, checked against the
    /// display it claims.
    ///
    /// Separate from [`Self::incoherences`] because the viewport is not part of
    /// the identity: it arrives from `--width` and `--height` at run time, and
    /// the contradiction only exists once the two meet. A window wider than the
    /// screen it is supposedly on is one subtraction away from being caught.
    pub fn check_viewport(&self, width: u32, height: u32) -> Option<Incoherence> {
        let screen = self.screen.as_ref()?;
        if width <= screen.width && height <= screen.height {
            return None;
        }
        Some(wrong(
            "screen",
            format!(
                "declares a {}x{} display, but the viewport is {width}x{height}; no window is larger than its screen",
                screen.width, screen.height
            ),
        ))
    }

    /// What this identity requires and this engine does not have.
    pub fn unmet(&self, caps: &crate::Capabilities) -> Vec<Requirement> {
        self.requires
            .iter()
            .copied()
            .filter(|need| !need.met_by(caps))
            .collect()
    }

    /// Refuse unless this identity is coherent *and* backed.
    ///
    /// The two failures are reported separately because they call for different
    /// responses: an incoherent identity is a file to fix, and an unmet
    /// requirement is an engine that cannot honour it. The same file may be
    /// perfectly good in front of a real-browser backend.
    pub fn admit(&self, caps: &crate::Capabilities) -> Result<(), H5iError> {
        let found = self.incoherences();
        if !found.is_empty() {
            let lines: Vec<String> = found.iter().map(|f| format!("  {f}")).collect();
            return Err(H5iError::Metadata(format!(
                "the browser identity `{}` contradicts itself:\n{}",
                self.name,
                lines.join("\n")
            )));
        }
        let unmet = self.unmet(caps);
        if !unmet.is_empty() {
            let lines: Vec<String> = unmet
                .iter()
                .map(|need| format!("  {}: {}", need.as_str(), need.why_unmet()))
                .collect();
            return Err(H5iError::Metadata(format!(
                "the browser identity `{}` needs what this engine does not have:\n{}\n\
                 It is not applied in part: an agent string claiming {} in front of an engine \
                 missing these is more detectable than no claim at all.",
                self.name,
                lines.join("\n"),
                self.browser.family.as_str(),
            )));
        }
        Ok(())
    }

    /// A stable short digest of everything this identity declares.
    ///
    /// Written into the session record and the receipts, for the reason the
    /// policy digest is: two sessions with the same digest presented the same
    /// browser, and that is a question an audit asks after the fact. Over the
    /// canonical JSON rather than the file, so two spellings of one identity
    /// digest alike and a reformatted file does not read as a changed identity.
    pub fn digest(&self) -> String {
        use sha2::{Digest, Sha256};
        let canonical = serde_json::to_string(self).unwrap_or_default();
        let mut hasher = Sha256::new();
        hasher.update(canonical.as_bytes());
        format!("{:x}", hasher.finalize())[..16].to_string()
    }

    /// Read an identity from a TOML file.
    ///
    /// Not validated here: a caller that wants to *show* someone what is wrong
    /// with their file needs it parsed first, and `identity check` is exactly
    /// that caller. [`Self::admit`] is the gate.
    pub fn from_toml(text: &str) -> Result<Self, H5iError> {
        toml::from_str(text)
            .map_err(|e| H5iError::Metadata(format!("that is not a browser identity: {e}")))
    }

    pub fn read(path: &Path) -> Result<Self, H5iError> {
        let text = std::fs::read_to_string(path).map_err(|e| {
            H5iError::Metadata(format!("could not read `{}`: {e}", path.display()))
        })?;
        Self::from_toml(&text)
    }

    /// The identity `selector` names: a built-in, or a path to a TOML file.
    ///
    /// A name is tried first and a path second, so a built-in can never be
    /// shadowed by a file that happens to sit in the working directory.
    pub fn resolve(selector: &str) -> Result<Self, H5iError> {
        if let Some(builtin) = builtin(selector) {
            return Ok(builtin);
        }
        let path = Path::new(selector);
        if path.exists() {
            return Self::read(path);
        }
        Err(H5iError::Metadata(format!(
            "no browser identity called `{selector}`, and no file at that path. \
             Built in: {}",
            builtins()
                .iter()
                .map(|i| i.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )))
    }
}

/// The identity this engine has always presented, now written down.
///
/// Every value here is what the engine sent or reported before this module
/// existed, and the test below holds it to that: `native` is the default, so a
/// change to it is a change to what every h5i user looks like.
pub fn native() -> Identity {
    Identity {
        name: "native".to_string(),
        mode: Mode::Native,
        browser: Browser {
            family: Family::H5i,
            version: env!("CARGO_PKG_VERSION").to_string(),
            user_agent: crate::net::USER_AGENT.to_string(),
            vendor: String::new(),
            product_sub: "20030107".to_string(),
        },
        device: Device {
            os: Os::Undeclared,
            platform: String::new(),
            oscpu: String::new(),
            mobile: false,
            max_touch_points: 0,
            hardware_concurrency: 1,
        },
        locale: Locale {
            // Two entries, and the second is a correction rather than an
            // addition. The wire has always said `en-US,en;q=0.9` while
            // `navigator.languages` said `["en-US"]`: the two answers to one
            // question, from two places, disagreeing by exactly the amount a
            // cross-layer check looks for. Building the header from this list
            // is what makes that impossible to write again; reproducing the
            // header byte for byte is what decides which of the two moves.
            languages: vec!["en-US".to_string(), "en".to_string()],
            timezone: None,
        },
        screen: None,
        requires: BTreeSet::new(),
    }
}

/// h5i, with what varies between installations taken out.
pub fn privacy() -> Identity {
    let major = env!("CARGO_PKG_VERSION")
        .split('.')
        .next()
        .unwrap_or("0")
        .to_string();
    Identity {
        name: "privacy".to_string(),
        mode: Mode::Privacy,
        browser: Browser {
            family: Family::H5i,
            version: major.clone(),
            user_agent: format!(
                "Mozilla/5.0 (compatible; h5i-browser/{major}; +https://github.com/h5i-dev/h5i)"
            ),
            vendor: String::new(),
            product_sub: "20030107".to_string(),
        },
        device: Device {
            os: Os::Undeclared,
            platform: String::new(),
            oscpu: String::new(),
            mobile: false,
            max_touch_points: 0,
            hardware_concurrency: 1,
        },
        locale: Locale {
            languages: vec!["en-US".to_string(), "en".to_string()],
            timezone: Some(TimeZone::utc()),
        },
        screen: None,
        requires: BTreeSet::new(),
    }
}

/// A Firefox identity this engine can actually back, end to end.
pub fn firefox_linux() -> Identity {
    Identity {
        name: "firefox-143-linux".to_string(),
        mode: Mode::Compatible,
        browser: Browser {
            family: Family::Firefox,
            version: "143.0".to_string(),
            user_agent: "Mozilla/5.0 (X11; Linux x86_64; rv:143.0) Gecko/20100101 Firefox/143.0"
                .to_string(),
            vendor: String::new(),
            product_sub: "20100101".to_string(),
        },
        device: Device {
            os: Os::Linux,
            platform: "Linux x86_64".to_string(),
            oscpu: "Linux x86_64".to_string(),
            mobile: false,
            max_touch_points: 0,
            hardware_concurrency: 8,
        },
        locale: Locale {
            languages: vec!["en-US".to_string(), "en".to_string()],
            timezone: Some(TimeZone::utc()),
        },
        screen: Some(Screen {
            width: 1920,
            height: 1080,
            avail_width: 1920,
            avail_height: 1053,
            color_depth: 24,
            device_pixel_ratio_x100: 100,
        }),
        // Every value it declares is only readable from script.
        requires: BTreeSet::from([Requirement::JavaScript]),
    }
}

/// A Chrome identity, shipped and refused.
///
/// It is here to be refused, and the refusal is the documentation: this is what
/// claiming Chrome costs, stated as requirements rather than discovered as a
/// blocked login. `h5i browser identity check chrome-151-windows` names each
/// missing piece, and the same file becomes usable unchanged the day a backend
/// can answer for them.
pub fn chrome_windows() -> Identity {
    Identity {
        name: "chrome-151-windows".to_string(),
        mode: Mode::Compatible,
        browser: Browser {
            family: Family::Chrome,
            version: "151.0.0.0".to_string(),
            user_agent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                         (KHTML, like Gecko) Chrome/151.0.0.0 Safari/537.36"
                .to_string(),
            vendor: "Google Inc.".to_string(),
            product_sub: "20030107".to_string(),
        },
        device: Device {
            os: Os::Windows,
            platform: "Win32".to_string(),
            oscpu: String::new(),
            mobile: false,
            max_touch_points: 0,
            hardware_concurrency: 8,
        },
        locale: Locale {
            languages: vec!["en-US".to_string(), "en".to_string()],
            timezone: Some(TimeZone::utc()),
        },
        screen: Some(Screen {
            width: 1920,
            height: 1080,
            avail_width: 1920,
            avail_height: 1032,
            color_depth: 24,
            device_pixel_ratio_x100: 100,
        }),
        requires: BTreeSet::from([
            Requirement::JavaScript,
            // The two Chrome cannot be claimed without. Client hints because
            // Chrome sends them on every request and exposes the matching
            // object; WebGL because a Chrome with no renderer string is a
            // combination that exists nowhere.
            Requirement::UaClientHints,
            Requirement::WebGl2,
        ]),
    }
}

/// Every identity that ships with the engine.
pub fn builtins() -> Vec<Identity> {
    vec![native(), privacy(), firefox_linux(), chrome_windows()]
}

/// The built-in `name` names, if it is one.
pub fn builtin(name: &str) -> Option<Identity> {
    builtins().into_iter().find(|i| i.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scripted() -> crate::Capabilities {
        crate::Capabilities::with_script(true)
    }

    #[test]
    fn every_builtin_is_coherent_with_itself() {
        for identity in builtins() {
            let found = identity.incoherences();
            assert!(
                found.is_empty(),
                "the built-in `{}` contradicts itself: {:?}",
                identity.name,
                found
            );
        }
    }

    #[test]
    fn native_is_exactly_what_this_engine_already_sent() {
        // The wire is the contract this test is protecting. `native` is the
        // default, so anything that moves here moves for every h5i user at
        // once, and it must be a decision rather than a drift.
        let native = native();
        assert_eq!(native.browser.user_agent, crate::net::USER_AGENT);
        assert_eq!(native.locale.accept_language(), crate::net::NATIVE_ACCEPT_LANGUAGE);
        // And the list the header is built from is the list `navigator` will
        // report, which is the disagreement this whole module exists to make
        // unwritable: the header said `en` was acceptable while script said it
        // was not.
        assert_eq!(native.locale.languages, ["en-US", "en"]);
        // Undeclared everywhere it was undeclared before: no screen object, no
        // pinned zone, an empty platform.
        assert!(native.screen.is_none());
        assert!(native.locale.timezone.is_none());
        assert!(native.device.platform.is_empty());
        assert_eq!(native.device.hardware_concurrency, 1);
        // And it needs nothing, so it is admitted even with script off.
        native.admit(&crate::Capabilities::current()).unwrap();
    }

    #[test]
    fn privacy_pins_the_two_values_that_vary_between_installs() {
        let privacy = privacy();
        // The patch version is gone from the agent string, which is the point:
        // it is what splits h5i users into release cohorts.
        assert!(!privacy.browser.user_agent.contains(env!("CARGO_PKG_VERSION")));
        assert!(privacy.browser.user_agent.contains("h5i-browser/0"));
        // And the host's zone is replaced by one, rather than left to leak.
        assert_eq!(privacy.locale.timezone.as_ref().unwrap().offset_minutes, 0);
        // Still h5i. Privacy is not a disguise, and the validator agrees.
        assert_eq!(privacy.browser.family, Family::H5i);
        privacy.admit(&crate::Capabilities::current()).unwrap();
    }

    #[test]
    fn a_chrome_claim_is_refused_rather_than_half_applied() {
        let chrome = chrome_windows();
        // Coherent as a description. Nothing in the file contradicts anything
        // else in it. It is the *engine* that cannot stand behind it.
        assert!(chrome.incoherences().is_empty());

        let unmet = chrome.unmet(&scripted());
        assert!(unmet.contains(&Requirement::UaClientHints));
        assert!(unmet.contains(&Requirement::WebGl2));

        let refused = chrome.admit(&scripted()).unwrap_err().to_string();
        assert!(refused.contains("ua-client-hints"), "{refused}");
        assert!(refused.contains("webgl2"), "{refused}");
        // The refusal explains itself: partly applying it is the worse outcome.
        assert!(refused.contains("more detectable"), "{refused}");
    }

    #[test]
    fn a_firefox_claim_this_engine_can_back_is_admitted_only_with_script() {
        let firefox = firefox_linux();
        firefox.admit(&scripted()).unwrap();
        // With script off nothing can read `navigator`, so the identity would
        // be a header and nothing behind it. Refused, and it says which flag.
        let refused = firefox
            .admit(&crate::Capabilities::current())
            .unwrap_err()
            .to_string();
        assert!(refused.contains("--script is off"), "{refused}");
    }

    #[test]
    fn the_windows_mac_contradiction_is_caught() {
        // The example the design started from: a Windows agent string in front
        // of a `navigator.platform` from another operating system.
        let mut identity = chrome_windows();
        identity.device.platform = "MacIntel".to_string();
        let found = identity.incoherences();
        assert!(
            found.iter().any(|f| f.field == "device.platform"),
            "{found:?}"
        );
    }

    #[test]
    fn a_mobile_identity_with_no_touch_points_is_caught() {
        let mut identity = chrome_windows();
        identity.device.os = Os::Android;
        identity.device.platform = "Linux armv8l".to_string();
        identity.device.mobile = true;
        identity.device.max_touch_points = 0;
        identity.browser.user_agent =
            "Mozilla/5.0 (Linux; Android 15) AppleWebKit/537.36 (KHTML, like Gecko) \
             Chrome/151.0.0.0 Mobile Safari/537.36"
                .to_string();
        let found = identity.incoherences();
        assert!(
            found.iter().any(|f| f.field == "device.max_touch_points"),
            "{found:?}"
        );
    }

    #[test]
    fn an_agent_string_that_does_not_carry_its_own_version_is_caught() {
        let mut identity = chrome_windows();
        identity.browser.version = "150.0.0.0".to_string();
        let found = identity.incoherences();
        assert!(
            found.iter().any(|f| f.field == "browser.user_agent"),
            "{found:?}"
        );
    }

    #[test]
    fn firefox_may_not_carry_chromes_token_or_chromes_product_sub() {
        let mut identity = firefox_linux();
        identity.browser.user_agent.push_str(" Chrome/151.0.0.0");
        identity.browser.product_sub = "20030107".to_string();
        let found = identity.incoherences();
        assert!(
            found.iter().any(|f| f.field == "browser.user_agent"),
            "{found:?}"
        );
        assert!(
            found.iter().any(|f| f.field == "browser.product_sub"),
            "{found:?}"
        );
    }

    #[test]
    fn only_h5i_may_leave_its_system_undeclared() {
        let mut identity = firefox_linux();
        identity.device.os = Os::Undeclared;
        identity.device.platform = String::new();
        let found = identity.incoherences();
        assert!(found.iter().any(|f| f.field == "device.os"), "{found:?}");
    }

    #[test]
    fn a_mode_may_not_claim_the_wrong_browser() {
        let mut honest = native();
        honest.browser.family = Family::Chrome;
        assert!(
            honest.incoherences().iter().any(|f| f.field == "mode"),
            "native may not claim Chrome"
        );

        let mut pretending = firefox_linux();
        pretending.mode = Mode::Privacy;
        assert!(
            pretending.incoherences().iter().any(|f| f.field == "mode"),
            "privacy may not claim Firefox"
        );
    }

    #[test]
    fn a_zone_that_observes_daylight_saving_is_refused_with_the_reason() {
        let mut identity = firefox_linux();
        identity.locale.timezone = Some(TimeZone {
            name: "America/New_York".to_string(),
            offset_minutes: -300,
        });
        let found = identity.incoherences();
        let zone = found
            .iter()
            .find(|f| f.field == "locale.timezone")
            .expect("the zone should be refused");
        assert!(zone.says.contains("daylight saving"), "{zone:?}");
        // And a zone that genuinely never shifts is accepted.
        identity.locale.timezone = TimeZone::named("Asia/Tokyo");
        assert_eq!(
            identity.locale.timezone.as_ref().unwrap().offset_minutes,
            540
        );
        assert!(identity.incoherences().is_empty());
    }

    #[test]
    fn an_offset_that_disagrees_with_its_own_zone_name_is_refused() {
        // The file says Tokyo and then writes New York's offset. Taking the
        // name on trust would give a page a `getTimezoneOffset` that no Tokyo
        // browser has ever returned.
        let mut identity = firefox_linux();
        identity.locale.timezone = Some(TimeZone {
            name: "Asia/Tokyo".to_string(),
            offset_minutes: -300,
        });
        assert!(
            identity
                .incoherences()
                .iter()
                .any(|f| f.field == "locale.timezone")
        );
    }

    #[test]
    fn a_viewport_larger_than_the_declared_screen_is_caught() {
        let identity = firefox_linux();
        // Inside the 1920x1080 it declares.
        assert!(identity.check_viewport(1280, 720).is_none());
        // And past it.
        let over = identity
            .check_viewport(2560, 1440)
            .expect("a window cannot be wider than its screen");
        assert!(over.says.contains("1920x1080"), "{over:?}");
        // An identity that declares no screen has nothing to contradict.
        assert!(native().check_viewport(9999, 9999).is_none());
    }

    #[test]
    fn accept_language_is_built_from_the_same_list_navigator_reports() {
        let locale = Locale {
            languages: vec!["ja".to_string(), "en-US".to_string(), "en".to_string()],
            timezone: None,
        };
        assert_eq!(locale.accept_language(), "ja,en-US;q=0.9,en;q=0.8");
    }

    #[test]
    fn more_languages_than_a_header_can_carry_are_refused() {
        // Found by running it: twelve declared, ten on the wire, twelve in
        // `navigator.languages`. Admitted, and incoherent. By this module's
        // own accessor, which is the worst place for it to come from.
        let mut identity = firefox_linux();
        identity.locale.languages = (0..12).map(|n| format!("l{n}")).collect();
        let found = identity.incoherences();
        assert!(
            found.iter().any(|f| f.field == "locale.languages"),
            "{found:?}"
        );

        // At the cap it is fine, and the two agree: one entry per language.
        identity.locale.languages = (0..MAX_LANGUAGES).map(|n| format!("l{n}")).collect();
        assert!(identity.incoherences().is_empty());
        assert_eq!(
            identity.locale.accept_language().split(',').count(),
            identity.locale.languages.len(),
            "the header and the array must describe the same list"
        );
    }

    /// Every built-in, and every list the header can carry, says the same thing
    /// twice.
    #[test]
    fn the_header_and_navigator_languages_never_disagree() {
        for identity in builtins() {
            assert_eq!(
                identity.locale.accept_language().split(',').count(),
                identity.locale.languages.len(),
                "`{}` sends a different number of languages than it reports",
                identity.name
            );
        }
    }

    #[test]
    fn a_language_tag_that_is_not_one_is_caught() {
        let mut identity = firefox_linux();
        identity.locale.languages = vec!["en_US".to_string()];
        assert!(
            identity
                .incoherences()
                .iter()
                .any(|f| f.field == "locale.languages")
        );
    }

    #[test]
    fn an_agent_string_that_could_not_be_a_header_is_caught() {
        let mut identity = native();
        identity.browser.user_agent = "Mozilla/5.0 h5i-browser/0.1.0\r\nX-Injected: 1"
            .to_string();
        let found = identity.incoherences();
        assert!(
            found
                .iter()
                .any(|f| f.field == "browser.user_agent" && f.says.contains("control")),
            "{found:?}"
        );
    }

    #[test]
    fn an_impossible_display_is_caught() {
        let mut identity = firefox_linux();
        let screen = identity.screen.as_mut().unwrap();
        screen.avail_width = 3840;
        screen.color_depth = 16;
        screen.device_pixel_ratio_x100 = 1000;
        let found = identity.incoherences();
        for field in [
            "screen.avail_width",
            "screen.color_depth",
            "screen.device_pixel_ratio_x100",
        ] {
            assert!(found.iter().any(|f| f.field == field), "{field}: {found:?}");
        }
    }

    #[test]
    fn the_digest_follows_the_values_and_not_the_spelling() {
        let one = firefox_linux();
        let mut two = firefox_linux();
        assert_eq!(one.digest(), two.digest());
        two.device.hardware_concurrency = 16;
        assert_ne!(one.digest(), two.digest());
        // Short enough to sit on a placement line, long enough not to collide.
        assert_eq!(one.digest().len(), 16);
    }

    #[test]
    fn an_identity_round_trips_through_toml() {
        let original = firefox_linux();
        let text = toml::to_string(&original).unwrap();
        let read = Identity::from_toml(&text).unwrap();
        assert_eq!(read, original);
        assert_eq!(read.digest(), original.digest());
    }

    #[test]
    fn resolve_prefers_a_built_in_to_a_file_of_the_same_name() {
        assert_eq!(Identity::resolve("native").unwrap(), native());
        let unknown = Identity::resolve("does-not-exist").unwrap_err().to_string();
        // The error lists what there is, rather than only saying no.
        assert!(unknown.contains("privacy"), "{unknown}");
        assert!(unknown.contains("firefox-143-linux"), "{unknown}");
    }

    #[test]
    fn a_requirement_is_written_the_way_it_is_printed() {
        // `identity check` prints `as_str`, and a file is parsed by serde. If
        // the two spell a name differently then the output tells someone to
        // write a word the parser refuses, which is what `rename_all` did:
        // `webgl2` printed, `web-gl2` accepted.
        for requirement in [
            Requirement::UaClientHints,
            Requirement::WebGl2,
            Requirement::WebRtc,
            Requirement::AudioContext,
            Requirement::MediaDevices,
            Requirement::ServiceWorker,
            Requirement::Video,
            Requirement::Canvas2d,
            Requirement::WebSockets,
            Requirement::JavaScript,
        ] {
            let printed = requirement.as_str();
            let parsed: Requirement = serde_json::from_str(&format!("\"{printed}\""))
                .unwrap_or_else(|e| panic!("`{printed}` is printed but not accepted: {e}"));
            assert_eq!(parsed, requirement);
        }
    }

    #[test]
    fn what_it_does_not_cover_is_stated_rather_than_left_to_be_found() {
        // The claim is coherence, not invisibility, and the list that says so
        // is part of the type rather than a line in a README that can rot.
        let uncovered = Identity::DOES_NOT_COVER.join(" ");
        assert!(uncovered.contains("ClientHello"));
        assert!(uncovered.contains("HTTP/2"));
        assert!(uncovered.contains("font"));
    }
}
