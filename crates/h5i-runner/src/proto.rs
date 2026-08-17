//! What the frames mean.
//!
//! [`crate::wire`] moves bodies; this module says what a type code is and what
//! its payload deserialises to. The split is deliberate: an unknown type code
//! is a framing success and a meaning failure, and only the second one ends the
//! session.
//!
//! Two rules run through everything here.
//!
//! **`HELLO` is static, `PROBE` is dynamic, and neither does the other's job**
//! (ROADMAP.md R5). The handshake carries what cannot change while a worker
//! binary sits on disk — the protocol version, the h5i version, the
//! architecture. Everything that drifts between one minute and the next — how
//! much memory is free, whether podman is installed today, which tiers actually
//! verify — belongs to [`Capabilities`] and arrives only in answer to a
//! `PROBE`. A field in the wrong one of those two is a field that goes stale in
//! a cache and lies later.
//!
//! **Identity never rides in a frame.** `runner_id` is computed on the client
//! from the host key SSH verified against the pinned `known_hosts`
//! ([`crate::identity`]). [`HelloAck::runner_id_echo`] exists so a mismatch can
//! be *noticed*, and is never the source of the value: what a peer asserts
//! about itself is exactly what pinning exists to make irrelevant.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use h5i_error::redact::sanitize_display;
use h5i_sandbox::sandbox_policy::IsolationClaim;

/// The protocol this build speaks.
///
/// Linear, and deliberately not negotiated: both ends state a number, the lower
/// one governs, and features are gated by named constants below rather than by
/// a capability handshake. E2B's SDKs do the same thing with semver constants,
/// and the constants file doubles as the protocol's changelog.
pub const PROTOCOL_VERSION: u16 = 1;

/// The oldest protocol this build can still talk to. Raise it only when a
/// change cannot be expressed as a gated feature, because raising it strands
/// every runner that has not been updated.
pub const MIN_PROTOCOL_VERSION: u16 = 1;

/// Longest string this protocol accepts in any peer-supplied field. Version
/// strings, architectures and refusal reasons are all short; anything longer is
/// either a bug or an attempt to make a terminal do something.
pub const MAX_STRING: usize = 512;

/// Longest error message and log tail. A create failure carries the tail of the
/// worker's log because a remote boot failure with no log is the worst place to
/// debug from, and that tail is the one peer-supplied string that is allowed to
/// be big.
pub const MAX_LOG_TAIL: usize = 16 * 1024;

/// Ceiling on any megabyte-denominated capability. A runner reporting more than
/// a petabyte of RAM is not a runner we should record numbers from.
pub const MAX_REPORTED_MB: u64 = 1024 * 1024 * 1024;

/// Most isolation tiers a capability report may list. The vocabulary has six
/// names; a peer sending hundreds is fuzzing, not advertising.
pub const MAX_ISOLATION_ENTRIES: usize = 16;

/// Frame type codes.
///
/// The numbering is grouped by phase (handshake, probe, box lifecycle, exec,
/// export, admin) with gaps left inside each group, so a later message lands
/// beside its relatives instead of at the end. R13.1 implements the first two
/// groups; the rest are declared here because the wire is easier to reason
/// about whole, and every one of them is refused with
/// [`ErrorCode::Unimplemented`] until its milestone lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FrameKind {
    Hello = 0x01,
    HelloAck = 0x02,
    Error = 0x0E,
    KeepAlive = 0x0F,

    Probe = 0x10,
    Capabilities = 0x11,

    CreateBox = 0x20,
    Data = 0x21,
    DataDone = 0x22,
    CreateResult = 0x23,

    Exec = 0x30,
    ExecStarted = 0x31,
    Stdout = 0x32,
    Stderr = 0x33,
    PtyOut = 0x34,
    Stdin = 0x35,
    PtyIn = 0x36,
    Resize = 0x37,
    Signal = 0x38,
    CloseStdin = 0x39,
    Exit = 0x3A,

    ExportBox = 0x40,
    ExportResult = 0x41,

    DestroyBox = 0x50,
    ListBoxes = 0x51,
    Gc = 0x52,
}

impl FrameKind {
    /// `None` for a code this build has no meaning for. The caller refuses the
    /// session; it does not guess, and it does not skip the frame and continue,
    /// because a peer sending codes we do not know is a peer we do not
    /// understand.
    pub fn from_u8(b: u8) -> Option<Self> {
        use FrameKind::*;
        Some(match b {
            0x01 => Hello,
            0x02 => HelloAck,
            0x0E => Error,
            0x0F => KeepAlive,
            0x10 => Probe,
            0x11 => Capabilities,
            0x20 => CreateBox,
            0x21 => Data,
            0x22 => DataDone,
            0x23 => CreateResult,
            0x30 => Exec,
            0x31 => ExecStarted,
            0x32 => Stdout,
            0x33 => Stderr,
            0x34 => PtyOut,
            0x35 => Stdin,
            0x36 => PtyIn,
            0x37 => Resize,
            0x38 => Signal,
            0x39 => CloseStdin,
            0x3A => Exit,
            0x40 => ExportBox,
            0x41 => ExportResult,
            0x50 => DestroyBox,
            0x51 => ListBoxes,
            0x52 => Gc,
            _ => return None,
        })
    }

    pub fn as_u8(self) -> u8 {
        self as u8
    }

    /// The name an operator reads in a refusal.
    pub fn as_str(self) -> &'static str {
        use FrameKind::*;
        match self {
            Hello => "HELLO",
            HelloAck => "HELLO_ACK",
            Error => "ERROR",
            KeepAlive => "KEEPALIVE",
            Probe => "PROBE",
            Capabilities => "CAPABILITIES",
            CreateBox => "CREATE_BOX",
            Data => "DATA",
            DataDone => "DATA_DONE",
            CreateResult => "CREATE_RESULT",
            Exec => "EXEC",
            ExecStarted => "EXEC_STARTED",
            Stdout => "STDOUT",
            Stderr => "STDERR",
            PtyOut => "PTY_OUT",
            Stdin => "STDIN",
            PtyIn => "PTY_IN",
            Resize => "RESIZE",
            Signal => "SIGNAL",
            CloseStdin => "CLOSE_STDIN",
            Exit => "EXIT",
            ExportBox => "EXPORT_BOX",
            ExportResult => "EXPORT_RESULT",
            DestroyBox => "DESTROY_BOX",
            ListBoxes => "LIST_BOXES",
            Gc => "GC",
        }
    }

    /// Is this a frame a *worker* should ever receive? A worker that is sent
    /// its own reply types is talking to something that is not a client.
    pub fn is_client_to_worker(self) -> bool {
        use FrameKind::*;
        matches!(
            self,
            Hello
                | Probe
                | CreateBox
                | Data
                | DataDone
                | Exec
                | Stdin
                | PtyIn
                | Resize
                | Signal
                | CloseStdin
                | ExportBox
                | DestroyBox
                | ListBoxes
                | Gc
                | KeepAlive
        )
    }
}

/// Why an exchange was refused. Stable strings, because they end up in receipts
/// and in tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ErrorCode {
    /// The peer's protocol version and ours cannot meet.
    ProtocolVersion,
    /// A frame arrived that this build has no meaning for.
    UnknownFrame,
    /// A frame arrived in a position the protocol does not allow it: anything
    /// before the handshake, a reply where a request belongs.
    Sequence,
    /// A payload did not deserialise, or deserialised to something invalid.
    Malformed,
    /// The request is well formed and names something this worker cannot do.
    Unsupported,
    /// The request is well formed and names a milestone that has not landed.
    Unimplemented,
    /// The worker failed while doing the work.
    Internal,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ProtocolVersion => "protocol-version",
            Self::UnknownFrame => "unknown-frame",
            Self::Sequence => "sequence",
            Self::Malformed => "malformed",
            Self::Unsupported => "unsupported",
            Self::Unimplemented => "unimplemented",
            Self::Internal => "internal",
        }
    }
}

/// What went wrong turning bytes into meaning.
#[derive(Debug, Error)]
pub enum ProtoError {
    #[error("frame type 0x{0:02X} is not one this h5i understands")]
    UnknownFrame(u8),

    #[error("expected {expected} and got {got}")]
    Unexpected {
        expected: &'static str,
        got: &'static str,
    },

    #[error("could not read a {kind} payload: {source}")]
    Malformed {
        kind: &'static str,
        #[source]
        source: serde_json::Error,
    },

    #[error("{0}")]
    Invalid(String),

    #[error(
        "this h5i speaks runner protocol {ours} and the other side speaks {theirs} — \
         update the older of the two"
    )]
    Version { ours: u16, theirs: u16 },

    /// The peer sent a well-formed [`FrameKind::Error`]. Carried as its own
    /// variant so a refusal reads as a refusal and not as a parse failure.
    #[error("{}: {}", .code.as_str(), .message)]
    Refused {
        code: ErrorCode,
        message: String,
        log_tail: Option<String>,
    },
}

impl ProtoError {
    /// The code this failure should be reported to a peer as.
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::UnknownFrame(_) => ErrorCode::UnknownFrame,
            Self::Unexpected { .. } => ErrorCode::Sequence,
            Self::Malformed { .. } | Self::Invalid(_) => ErrorCode::Malformed,
            Self::Version { .. } => ErrorCode::ProtocolVersion,
            Self::Refused { code, .. } => *code,
        }
    }
}

/// The client's opening frame. Static facts only.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hello {
    /// The highest protocol this client speaks.
    pub protocol: u16,
    /// Human-facing, for the worker's log. Never parsed for behaviour: features
    /// are gated on `protocol`, so that a version string cannot become a
    /// capability check by accident.
    pub h5i_version: String,
}

/// The worker's answer. Static facts only; see the module note.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelloAck {
    pub protocol: u16,
    pub h5i_version: String,
    /// `x86_64`, `aarch64`. Static for a given binary, and the one fact a
    /// client needs before it can decide whether an image will run at all.
    pub arch: String,
    /// `linux`, and refused as anything else: a runner is a Linux machine
    /// (ROADMAP.md R1).
    pub os: String,
    /// The worker's opinion of its own identity, for detecting a mismatch.
    /// Never the source of `runner_id`; see the module note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner_id_echo: Option<String>,
}

/// Everything that can change between one probe and the next.
///
/// This is the vocabulary R1's rule is written in: a runner requires Linux and
/// the h5i protocol, and *everything else is an advertised capability*. A box
/// that asks for something absent is refused with the capability named, never
/// silently given something weaker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capabilities {
    pub arch: String,
    pub os: String,
    /// Total system memory. Advisory: it bounds what a box may ask for, and
    /// says nothing about what is free right now.
    pub memory_mb: u64,
    /// Space available where boxes are built.
    pub workspace_mb: u64,
    /// The tiers this runner will actually run, in the vocabulary of
    /// [`IsolationClaim`]. Verified functionally, not inferred from which
    /// kernel bits are present: this codebase has paid once for the difference
    /// between "Landlock exists" and "a confined exec works".
    pub isolation: Vec<String>,
    /// Rootless podman is present and usable.
    pub container: bool,
    /// Hardware virtualisation is available for a future microvm placement.
    pub kvm: bool,
    /// Box state survives a disconnect and a reboot. False for a read-only OS
    /// with a tmpfs workspace, where a reboot is an early-expired lease
    /// (ROADMAP.md R11).
    pub persistent_boxes: bool,
    /// The runner reaches the internet itself, so image pulls and package
    /// installs can leave through its own allowlist proxy. False is the
    /// cable-only appliance, which needs brokered egress and is not an MVP
    /// topology (ROADMAP.md R12).
    pub own_egress: bool,
    /// What the worker could not determine, in its own words. Advisory text for
    /// an operator: a probe that silently reports `false` for something it
    /// merely could not measure is a probe that lies.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

impl Capabilities {
    /// Make a peer-supplied report safe to store and print, or refuse it.
    ///
    /// R13.1's exit criterion asks for hostile capability values to be clamped
    /// or refused and *never stored*, which is this function. The distinction
    /// it draws: a number that is merely implausible gets clamped, because a
    /// runner with a broken `/proc` should still be usable; a value that would
    /// change a *decision* — an isolation tier this h5i does not have a name
    /// for, an OS that is not Linux — is refused, because storing it would mean
    /// a later create consults a capability list that means nothing.
    pub fn sanitized(mut self) -> Result<Self, ProtoError> {
        self.arch = clean_field("arch", &self.arch)?;
        self.os = clean_field("os", &self.os)?;

        if self.os != "linux" {
            return Err(ProtoError::Invalid(format!(
                "a runner must be Linux, and this one reports `{}`",
                self.os
            )));
        }

        // Implausible sizes are clamped rather than refused: the number is
        // advisory, and a runner whose `/proc/meminfo` is unreadable is still a
        // runner.
        self.memory_mb = self.memory_mb.min(MAX_REPORTED_MB);
        self.workspace_mb = self.workspace_mb.min(MAX_REPORTED_MB);

        if self.isolation.len() > MAX_ISOLATION_ENTRIES {
            return Err(ProtoError::Invalid(format!(
                "capability report lists {} isolation tiers, over the {MAX_ISOLATION_ENTRIES} \
                 this protocol accepts",
                self.isolation.len()
            )));
        }
        let mut tiers = Vec::with_capacity(self.isolation.len());
        for raw in &self.isolation {
            let name = clean_field("isolation tier", raw)?;
            // Parsed against the product's own vocabulary rather than a list
            // spelled out here, so the protocol cannot drift from the tiers
            // h5i actually has.
            let claim = IsolationClaim::parse(&name).map_err(|e| {
                ProtoError::Invalid(format!("capability report names a tier h5i does not have: {e}"))
            })?;
            let canonical = claim.as_str().to_string();
            if !tiers.contains(&canonical) {
                tiers.push(canonical);
            }
        }
        self.isolation = tiers;

        // A tier claimed without the runtime that implements it is a
        // contradiction, and the honest reading is that the report is wrong
        // about something we are about to make placement decisions on.
        if self.isolation.iter().any(|t| t == "container") && !self.container {
            return Err(ProtoError::Invalid(
                "capability report claims the container tier and no container runtime".into(),
            ));
        }

        let mut notes = Vec::new();
        for note in self.notes.iter().take(MAX_ISOLATION_ENTRIES) {
            notes.push(clean_field("note", note)?);
        }
        self.notes = notes;

        Ok(self)
    }

    /// Does this runner advertise a tier by name?
    pub fn advertises(&self, claim: IsolationClaim) -> bool {
        self.isolation.iter().any(|t| t == claim.as_str())
    }
}

/// A refusal, on the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorMsg {
    pub code: ErrorCode,
    pub message: String,
    /// The tail of the worker-side log, when the failure is one an operator
    /// cannot diagnose from the message alone. bhatti paid for this lesson in
    /// bug reports; a remote boot failure with no log is the worst debugging
    /// position there is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_tail: Option<String>,
}

impl ErrorMsg {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            log_tail: None,
        }
    }

    pub fn with_log_tail(mut self, tail: impl Into<String>) -> Self {
        let tail = tail.into();
        let start = tail.len().saturating_sub(MAX_LOG_TAIL);
        self.log_tail = Some(tail[start..].to_string());
        self
    }

    /// Make a peer-supplied refusal safe to print. Called on receipt, because
    /// this string reaches a terminal.
    pub fn sanitized(mut self) -> Self {
        self.message = truncate(&sanitize_display(&self.message), MAX_STRING);
        self.log_tail = self
            .log_tail
            .as_deref()
            .map(|t| truncate(&h5i_error::redact::sanitize_block(t), MAX_LOG_TAIL));
        self
    }
}

/// Reject a peer-supplied string that is empty, over-long, or carrying terminal
/// control sequences. Sanitised rather than merely rejected because these
/// strings are printed, and a refusal that itself prints the hostile bytes has
/// not helped.
fn clean_field(what: &'static str, value: &str) -> Result<String, ProtoError> {
    if value.is_empty() {
        return Err(ProtoError::Invalid(format!("{what} is empty")));
    }
    if value.len() > MAX_STRING {
        return Err(ProtoError::Invalid(format!(
            "{what} is {} bytes, over the {MAX_STRING} byte limit",
            value.len()
        )));
    }
    let clean = sanitize_display(value);
    if clean.trim().is_empty() {
        return Err(ProtoError::Invalid(format!(
            "{what} is nothing but control characters"
        )));
    }
    Ok(clean)
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// The protocol both ends will speak, or a refusal naming both numbers.
///
/// No negotiation: the lower version governs. A worker too old to meet
/// [`MIN_PROTOCOL_VERSION`] fails here, at the handshake, with the numbers in
/// the message — not later, in the middle of a create, as a mysterious unknown
/// frame.
pub fn agreed_protocol(ours: u16, theirs: u16) -> Result<u16, ProtoError> {
    let agreed = ours.min(theirs);
    if agreed < MIN_PROTOCOL_VERSION {
        return Err(ProtoError::Version { ours, theirs });
    }
    Ok(agreed)
}

/// Serialise a message into a frame payload.
pub fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, ProtoError> {
    serde_json::to_vec(value).map_err(|source| ProtoError::Malformed {
        kind: "outgoing message",
        source,
    })
}

/// Deserialise a frame payload.
pub fn decode<T: for<'de> Deserialize<'de>>(
    kind: &'static str,
    payload: &[u8],
) -> Result<T, ProtoError> {
    serde_json::from_slice(payload).map_err(|source| ProtoError::Malformed { kind, source })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps() -> Capabilities {
        Capabilities {
            arch: "aarch64".into(),
            os: "linux".into(),
            memory_mb: 512,
            workspace_mb: 4096,
            isolation: vec!["process".into(), "supervised".into()],
            container: false,
            kvm: false,
            persistent_boxes: true,
            own_egress: true,
            notes: vec![],
        }
    }

    #[test]
    fn every_frame_kind_round_trips_through_its_code() {
        // A hand-written `from_u8` and a `#[repr(u8)]` enum are two spellings
        // of the same table, and the way they rot is silently.
        for byte in 0u8..=255 {
            if let Some(kind) = FrameKind::from_u8(byte) {
                assert_eq!(kind.as_u8(), byte, "{kind:?} disagrees with its own code");
                assert!(!kind.as_str().is_empty());
            }
        }
    }

    #[test]
    fn an_unknown_code_has_no_meaning() {
        assert!(FrameKind::from_u8(0x00).is_none());
        assert!(FrameKind::from_u8(0xEE).is_none());
        assert!(FrameKind::from_u8(0xFF).is_none());
    }

    #[test]
    fn the_lower_protocol_governs_and_too_old_is_named() {
        assert_eq!(agreed_protocol(1, 1).unwrap(), 1);
        assert_eq!(agreed_protocol(5, 2).unwrap(), 2);
        assert_eq!(agreed_protocol(2, 5).unwrap(), 2);
        match agreed_protocol(PROTOCOL_VERSION, 0) {
            Err(ProtoError::Version { ours, theirs }) => {
                assert_eq!((ours, theirs), (PROTOCOL_VERSION, 0));
            }
            other => panic!("expected a version refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_good_capability_report_survives_sanitising() {
        let c = caps().sanitized().expect("valid");
        assert_eq!(c.isolation, vec!["process", "supervised"]);
        assert!(c.advertises(IsolationClaim::Process));
        assert!(!c.advertises(IsolationClaim::Container));
    }

    #[test]
    fn an_absurd_size_is_clamped_not_stored() {
        let mut c = caps();
        c.memory_mb = u64::MAX;
        c.workspace_mb = u64::MAX;
        let c = c.sanitized().expect("clamped, not refused");
        assert_eq!(c.memory_mb, MAX_REPORTED_MB);
        assert_eq!(c.workspace_mb, MAX_REPORTED_MB);
    }

    #[test]
    fn a_tier_h5i_does_not_have_is_refused() {
        // The value that would change a decision: storing it means a later
        // create consults a capability list that means nothing.
        let mut c = caps();
        c.isolation = vec!["process".into(), "quantum".into()];
        assert!(matches!(c.sanitized(), Err(ProtoError::Invalid(_))));
    }

    #[test]
    fn a_tier_without_its_runtime_is_a_contradiction() {
        let mut c = caps();
        c.isolation.push("container".into());
        c.container = false;
        assert!(matches!(c.sanitized(), Err(ProtoError::Invalid(_))));
    }

    #[test]
    fn a_runner_that_is_not_linux_is_refused() {
        let mut c = caps();
        c.os = "darwin".into();
        assert!(matches!(c.sanitized(), Err(ProtoError::Invalid(_))));
    }

    #[test]
    fn control_sequences_never_reach_a_terminal() {
        // These strings are printed by `runner list` and `runner probe`. A peer
        // that can move the cursor can forge output around its own line.
        let mut c = caps();
        c.arch = "aarch64\x1b[2J\x1b[H".into();
        let c = c.sanitized().expect("sanitised, not refused");
        assert!(!c.arch.contains('\x1b'), "escape survived: {:?}", c.arch);

        // What survives is the printable remainder, not the escape: `\x1b[31m`
        // becomes the literal `[31m`, which is a strange architecture and a
        // harmless one. The refusal is for a field with nothing left at all.
        let mut c = caps();
        c.arch = "\x1b[31m".into();
        assert_eq!(c.sanitized().expect("printable remainder").arch, "[31m");

        let mut c = caps();
        c.arch = "\u{1b}\u{7}\u{0}".into();
        assert!(
            matches!(c.sanitized(), Err(ProtoError::Invalid(_))),
            "a field that is only control characters is not a value"
        );
    }

    #[test]
    fn an_overlong_field_is_refused() {
        let mut c = caps();
        c.arch = "a".repeat(MAX_STRING + 1);
        assert!(matches!(c.sanitized(), Err(ProtoError::Invalid(_))));
    }

    #[test]
    fn an_empty_field_is_refused() {
        let mut c = caps();
        c.arch = String::new();
        assert!(matches!(c.sanitized(), Err(ProtoError::Invalid(_))));
    }

    #[test]
    fn too_many_tiers_is_fuzzing_not_advertising() {
        let mut c = caps();
        c.isolation = vec!["process".into(); MAX_ISOLATION_ENTRIES + 1];
        assert!(matches!(c.sanitized(), Err(ProtoError::Invalid(_))));
    }

    #[test]
    fn duplicate_tiers_collapse() {
        let mut c = caps();
        c.isolation = vec!["process".into(), "PROCESS".into(), " process ".into()];
        let c = c.sanitized().unwrap();
        assert_eq!(c.isolation, vec!["process"]);
    }

    #[test]
    fn a_log_tail_keeps_its_end_and_is_bounded() {
        // The tail is what matters — the failure is at the end of the log, not
        // the start — and it is the one peer-supplied string allowed to be big.
        let long = "x".repeat(MAX_LOG_TAIL * 2) + "THE ACTUAL FAILURE";
        let e = ErrorMsg::new(ErrorCode::Internal, "create failed").with_log_tail(long);
        let tail = e.log_tail.as_deref().unwrap();
        assert!(tail.len() <= MAX_LOG_TAIL);
        assert!(tail.ends_with("THE ACTUAL FAILURE"));
    }

    #[test]
    fn a_refusal_is_sanitised_before_it_is_printed() {
        let e = ErrorMsg::new(ErrorCode::Internal, "boom\x1b[2Jgone")
            .with_log_tail("tail\x1b[2J")
            .sanitized();
        assert!(!e.message.contains('\x1b'));
        assert!(!e.log_tail.unwrap().contains('\x1b'));
    }

    #[test]
    fn a_worker_knows_which_frames_are_not_for_it() {
        assert!(FrameKind::Exec.is_client_to_worker());
        assert!(FrameKind::Hello.is_client_to_worker());
        // Its own reply types arriving inbound mean the peer is not a client.
        assert!(!FrameKind::HelloAck.is_client_to_worker());
        assert!(!FrameKind::Capabilities.is_client_to_worker());
        assert!(!FrameKind::ExecStarted.is_client_to_worker());
        assert!(!FrameKind::Exit.is_client_to_worker());
    }

    #[test]
    fn messages_round_trip_as_json() {
        let hello = Hello {
            protocol: PROTOCOL_VERSION,
            h5i_version: "0.3.4".into(),
        };
        let bytes = encode(&hello).unwrap();
        let back: Hello = decode("HELLO", &bytes).unwrap();
        assert_eq!(back.protocol, PROTOCOL_VERSION);

        let bytes = encode(&caps()).unwrap();
        let back: Capabilities = decode("CAPABILITIES", &bytes).unwrap();
        assert_eq!(back.memory_mb, 512);
    }

    #[test]
    fn a_payload_that_is_not_json_is_malformed_not_a_panic() {
        let e = decode::<Hello>("HELLO", b"{not json").unwrap_err();
        assert_eq!(e.code(), ErrorCode::Malformed);
    }
}
