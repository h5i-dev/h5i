//! What the frames mean.
//! [`crate::wire`] moves bodies; this module says what a type code is and what
//! its payload deserialises to. The split is deliberate: an unknown type code
//! is a framing success and a meaning failure, and only the second one ends the
//! session.
//! Two rules run through everything here.
//! `HELLO` is static, `PROBE` is dynamic, and neither does the other's job
//! (design-runner.md R5). The handshake carries what cannot change while a
//! worker binary sits on disk: the protocol version, the h5i version, the
//! architecture. Everything that drifts between one minute and the next belongs
//! to [`Capabilities`] and arrives only in answer to a `PROBE`. A field in the
//! wrong one of those two goes stale in a cache and lies later.
//! Identity never rides in a frame. `runner_id` is computed on the client from
//! the host key SSH verified against the pinned `known_hosts`
//! ([`crate::identity`]). [`HelloAck::runner_id_echo`] exists so a mismatch can
//! be *noticed*, and is never the source of the value.

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
    ExportDone = 0x41,

    DestroyBox = 0x50,
    ListBoxes = 0x51,
    Gc = 0x52,
    DestroyResult = 0x53,
    BoxList = 0x54,
    GcResult = 0x55,
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
            0x41 => ExportDone,
            0x50 => DestroyBox,
            0x51 => ListBoxes,
            0x52 => Gc,
            0x53 => DestroyResult,
            0x54 => BoxList,
            0x55 => GcResult,
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
            ExportDone => "EXPORT_RESULT",
            DestroyBox => "DESTROY_BOX",
            ListBoxes => "LIST_BOXES",
            Gc => "GC",
            DestroyResult => "DESTROY_RESULT",
            BoxList => "BOX_LIST",
            GcResult => "GC_RESULT",
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
    /// (design-runner.md R1).
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
    /// (design-runner.md R11).
    pub persistent_boxes: bool,
    /// The runner reaches the internet itself, so image pulls and package
    /// installs can leave through its own allowlist proxy. False is the
    /// cable-only appliance, which needs brokered egress and is not an MVP
    /// topology (design-runner.md R12).
    pub own_egress: bool,
    /// What the worker could not determine, in its own words. Advisory text for
    /// an operator: a probe that silently reports `false` for something it
    /// merely could not measure is a probe that lies.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

impl Capabilities {
    /// Make a peer-supplied report safe to store and print, or refuse it.
    /// R13.1's exit criterion asks for hostile capability values to be clamped
    /// or refused and *never stored*. The distinction it draws: a number that
    /// is merely implausible gets clamped, because a runner with a broken
    /// `/proc` should still be usable; a value that would change a *decision*,
    /// an isolation tier this h5i does not have a name for or an OS that is not
    /// Linux, is refused, because storing it would mean a later create consults
    /// a capability list that means nothing.
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

// ─── R13.2: create, destroy, list, gc ────────────────────────────────────────

/// How large a source transfer may be, in bytes.
///
/// A repository bundle, not a disk image: a bundle of a shallow base commit is
/// tens of megabytes for most projects. The cap is here so that a receiver
/// bounds the transfer by its own number rather than by whatever the sender
/// declares, which is R5's rule applied to the one RPC that moves real data.
pub const MAX_SOURCE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Identifiers that become directory names on the runner. Bounded so a peer
/// cannot make a path we then have to reason about.
pub const MAX_ID: usize = 128;

/// A SHA-256, lowercase hex.
pub const DIGEST_HEX: usize = 64;

/// What a box may consume on the runner.
///
/// Enforceable at runtime rather than only at boot, which is the one place a
/// container placement cannot copy a microVM's design: a microVM's limits are
/// its virtual hardware, and a container's are cgroups that have to be asked
/// for explicitly.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct ResourceLimits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_mb: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub procs: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wall_secs: Option<u64>,
}

/// How long the box lives without being touched.
/// There is no daemon on the runner to watch a clock (design-runner.md R11), so
/// a lease is a fact on disk that any later invocation can evaluate. `ttl_secs`
/// is refreshed by any RPC that touches the box; `hard_ttl_secs` is measured
/// from creation and is not.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct LeaseSpec {
    pub ttl_secs: u64,
    pub hard_ttl_secs: u64,
}

impl Default for LeaseSpec {
    fn default() -> Self {
        Self {
            ttl_secs: 2 * 60 * 60,
            hard_ttl_secs: 12 * 60 * 60,
        }
    }
}

/// Where a box's source comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceKind {
    /// A `git bundle` follows as `DATA` frames. The bundle *is* the base
    /// identity, verifiable on receipt, which is why R7 chose it over a tar.
    GitBundle,
    /// Nothing follows. An empty box, for a `--new` source.
    Empty,
}

/// The source transfer that follows a [`CreateRequest`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceSpec {
    pub kind: SourceKind,
    /// Declared size. A claim, not a budget: the receiver stops at its own cap
    /// and at this number, whichever is smaller.
    pub bytes: u64,
    /// SHA-256 of the bundle, lowercase hex. Verified before a single git
    /// command is run against it.
    pub sha256: String,
    /// The commit the box is based on, checked out after the bundle is cloned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_commit: Option<String>,
}

/// Make a box.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateRequest {
    /// The box's name on the runner. Becomes a directory, so it is
    /// path-checked.
    pub box_id: String,
    /// This attempt's id. The worker builds under `creating/<operation_id>` and
    /// renames into `live/<box_id>`, so a crash leaves a directory that names
    /// the attempt rather than a half-built box wearing the real name.
    pub operation_id: String,
    /// A digest over everything in this request that decides what gets built.
    /// The idempotency key (design-runner.md R7): re-sending a create whose
    /// digest matches an existing box returns that box, so a lost
    /// `CREATE_RESULT` costs a retry rather than a duplicate. A *different*
    /// digest under the same `box_id` is refused, because it asks for a
    /// different box under a name that is taken.
    pub request_digest: String,
    /// The tier this box wants. Refused when the runner does not advertise it
    /// (R1), never quietly downgraded.
    pub isolation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(default)]
    pub limits: ResourceLimits,
    #[serde(default)]
    pub lease: LeaseSpec,
    /// Digest of the resolved policy. The worker echoes it in
    /// [`CreateResult::policy_digest`] and the host refuses to mark the box
    /// live unless it matches, which turns "the worker silently enforced an
    /// older policy" from a possibility into a detected fault.
    pub policy_digest: String,
    /// The resolved policy itself, opaque to this protocol.
    ///
    /// `h5i-sandbox` owns that schema and this layer deliberately does not
    /// mirror it: a protocol that restated the policy's shape would be a second
    /// definition to keep in step with the first.
    pub policy: serde_json::Value,
    pub source: SourceSpec,
}

/// What a box is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BoxState {
    /// Being built. Only ever seen by a sweep: a create either renames into
    /// `Live` or leaves this behind for the next invocation to reap.
    Creating,
    Live,
    /// Its lease ran out and it has not been reaped yet.
    Expired,
}

impl BoxState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Creating => "creating",
            Self::Live => "live",
            Self::Expired => "expired",
        }
    }
}

/// A box was made, or already existed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateResult {
    pub box_id: String,
    /// True when this was an idempotent re-send and the box was already there.
    /// Named rather than silent, so a client can tell "I made this" from "this
    /// was already made" without inferring it from timing.
    pub existing: bool,
    /// The digest of the policy the worker actually stored (R7 step 4).
    pub policy_digest: String,
    /// Where the box's workspace is on the runner, for diagnosis.
    pub workspace: String,
    /// The container backing it, when the tier has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
    pub lease_expires_at: chrono::DateTime<chrono::Utc>,
}

impl CreateResult {
    /// Make a worker's answer safe to store and print.
    ///
    /// The runner is the machine this design agreed might be compromised, so
    /// everything it says about itself is peer data. The same category as a
    /// manifest pulled from someone else's clone, and handled the same way.
    /// Without this, a `workspace` path full of escape sequences reaches a
    /// terminal and a `box_id` of unbounded length reaches a receipt.
    pub fn sanitized(mut self) -> Result<Self, ProtoError> {
        check_id("box id", &self.box_id)?;
        check_digest("policy digest", &self.policy_digest)?;
        self.workspace = clean_field("workspace", &self.workspace)?;
        self.container = match self.container {
            Some(c) => Some(clean_field("container", &c)?),
            None => None,
        };
        Ok(self)
    }
}

impl ExecStarted {
    /// The `cwd` here is written into a receipt and rendered by `box status`,
    /// the console and `receipt render`. A runner that put escape sequences in
    /// it could forge lines around its own in every one of them.
    ///
    /// `timeout_secs` is clamped for a sharper reason: the client arms its own
    /// watchdog from it, so an unclamped `u64::MAX` is a peer choosing how long
    /// we wait, which is to say, forever. A number a peer supplies is never a
    /// clock; it is a claim about a clock, and it is bounded by ours.
    pub fn sanitized(mut self) -> Result<Self, ProtoError> {
        self.cwd = clean_field("working directory", &self.cwd)?;
        self.timeout_secs = self.timeout_secs.clamp(1, EXEC_MAX_SECS);
        Ok(self)
    }
}

impl ExportResult {
    pub fn sanitized(self) -> Result<Self, ProtoError> {
        check_id("box id", &self.box_id)?;
        // These two are compared against what actually arrives, so a bad value
        // fails that comparison rather than being trusted, but they are also
        // displayed, and an object id that is not one is worth refusing where
        // it is cheap.
        check_hex("tip commit", &self.tip_commit, 40, 64)?;
        check_hex("tip tree", &self.tip_tree, 40, 64)?;
        if self.bytes > MAX_SOURCE_BYTES {
            return Err(ProtoError::Invalid(format!(
                "the runner describes an export of {} bytes, over the {MAX_SOURCE_BYTES} limit",
                self.bytes
            )));
        }
        check_digest("export digest", &self.sha256)?;
        Ok(self)
    }
}

impl ListResult {
    /// A hostile runner's box list is still a list this side prints.
    pub fn sanitized(mut self) -> Result<Self, ProtoError> {
        if self.boxes.len() > MAX_LISTED_BOXES {
            return Err(ProtoError::Invalid(format!(
                "the runner listed {} boxes, over the {MAX_LISTED_BOXES} this protocol accepts",
                self.boxes.len()
            )));
        }
        for b in &mut self.boxes {
            check_id("box id", &b.box_id)?;
            // Empty is legal here: a `creating/` entry has no tier yet.
            if !b.isolation.is_empty() {
                b.isolation = clean_field("isolation", &b.isolation)?;
            }
            if !b.policy_digest.is_empty() {
                check_digest("policy digest", &b.policy_digest)?;
            }
            b.container = match b.container.take() {
                Some(c) => Some(clean_field("container", &c)?),
                None => None,
            };
        }
        Ok(self)
    }
}

impl DestroyResult {
    pub fn sanitized(mut self) -> Result<Self, ProtoError> {
        check_id("box id", &self.box_id)?;
        self.warnings.truncate(MAX_ISOLATION_ENTRIES);
        for w in &mut self.warnings {
            *w = truncate(&sanitize_display(w), MAX_STRING);
        }
        Ok(self)
    }
}

impl GcResult {
    pub fn sanitized(mut self) -> Result<Self, ProtoError> {
        self.reaped.truncate(MAX_LISTED_BOXES);
        self.kept.truncate(MAX_LISTED_BOXES);
        for r in &mut self.reaped {
            check_id("box id", r)?;
        }
        // `kept` carries a reason, so it is text rather than an identifier.
        for k in &mut self.kept {
            *k = truncate(&sanitize_display(k), MAX_STRING);
        }
        Ok(self)
    }
}

/// Most boxes a runner may claim to hold. A peer listing millions is fuzzing.
pub const MAX_LISTED_BOXES: usize = 4096;

/// Remove a box.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DestroyRequest {
    pub box_id: String,
    /// Remove the record even when the container could not be stopped.
    #[serde(default)]
    pub force: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DestroyResult {
    pub box_id: String,
    /// False when there was nothing there. Not an error: destroying something
    /// already gone is the state the caller asked for, and a retry after a lost
    /// answer must not fail.
    pub existed: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// One box, as the runner sees it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoxSummary {
    pub box_id: String,
    pub state: BoxState,
    pub isolation: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub lease_expires_at: chrono::DateTime<chrono::Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
    pub policy_digest: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ListResult {
    pub boxes: Vec<BoxSummary>,
}

/// Reap what the leases say is over.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct GcRequest {
    /// Reap every box, not only the expired ones.
    #[serde(default)]
    pub all: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GcResult {
    pub reaped: Vec<String>,
    /// Boxes a sweep decided to keep despite an expired lease, with the reason.
    /// A silent skip would read as "there was nothing to do" (design-runner.md
    /// R11: an un-reaped live box beats an unrecoverable dead one, but not
    /// silently).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kept: Vec<String>,
}

// ─── R13.3: exec ─────────────────────────────────────────────────────────────

/// How much captured output one exec may return.
///
/// A build's log, not an artifact: anything past this is truncated with a note
/// rather than refused, because a run that produced too much output still
/// produced an exit code worth having.
pub const MAX_EXEC_OUTPUT: usize = 8 * 1024 * 1024;

/// What one exec exchange may consume.
///
/// Twice [`MAX_EXEC_OUTPUT`] because stdout and stderr are each capped at it,
/// plus room for the framing and the two control messages around them. A
/// client whose budget was smaller than what the worker is allowed to send
/// would refuse its own legitimate results, which is exactly the bug this
/// number exists to not have.
pub fn exec_limits() -> crate::wire::Limits {
    crate::wire::Limits::bulk((MAX_EXEC_OUTPUT as u64) * 2 + 4 * 1024 * 1024)
}

/// Run something in a box.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecRequest {
    pub box_id: String,
    /// The command, as an argv array. Never a shell string: a shell is
    /// something a caller asks for by name, not something the protocol implies.
    pub argv: Vec<String>,
    /// Working directory, relative to the box's workspace. Absolute paths and
    /// anything that climbs out are refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Extra environment, already filtered by the caller's policy.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<(String, String)>,
    /// Wall-clock bound. The worker clamps this to its own maximum: a client's
    /// number is a request, and the receiver's is the budget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
}

impl ExecRequest {
    pub fn validated(&self) -> Result<(), ProtoError> {
        check_id("box id", &self.box_id)?;
        if self.argv.is_empty() {
            return Err(ProtoError::Invalid("an exec with no command".into()));
        }
        if self.argv.len() > 4096 {
            return Err(ProtoError::Invalid(format!(
                "an exec with {} arguments is not a command",
                self.argv.len()
            )));
        }
        // The cwd becomes a path under the box's workspace on a machine we do
        // not own, so it is checked the way every other peer-supplied path
        // component is: by shape, and totally.
        if let Some(cwd) = &self.cwd {
            let bad = cwd.starts_with('/')
                || cwd.starts_with('~')
                || cwd.split('/').any(|seg| seg == ".." || seg.contains('\0'))
                || cwd.len() > 1024;
            if bad {
                return Err(ProtoError::Invalid(format!(
                    "`{}` is not a working directory inside the box — it must be a relative \
                     path that stays under the workspace",
                    sanitize_display(cwd)
                )));
            }
        }
        if self.env.len() > 4096 {
            return Err(ProtoError::Invalid(format!(
                "an exec with {} environment entries is not a command",
                self.env.len()
            )));
        }
        for (k, v) in &self.env {
            // A NUL in either half is refused, not only in the name: both
            // become bytes in an `execve` array, and a validator that checks
            // one and not the other is a validator someone will trust for both.
            if k.is_empty() || k.len() > MAX_STRING || k.contains('=') || k.contains('\0') {
                return Err(ProtoError::Invalid(format!(
                    "`{}` is not an environment variable name",
                    truncate(&sanitize_display(k), 64)
                )));
            }
            if v.contains('\0') {
                return Err(ProtoError::Invalid(format!(
                    "the value of `{}` contains a NUL byte",
                    truncate(&sanitize_display(k), 64)
                )));
            }
        }
        for a in &self.argv {
            if a.contains('\0') {
                return Err(ProtoError::Invalid(
                    "an argument contains a NUL byte, which no argv can carry".into(),
                ));
            }
        }
        Ok(())
    }
}

/// The worker's hard ceiling on one command, known to both sides.
///
/// Both sides, deliberately: the worker clamps a request to it, and the client
/// clamps the worker's *answer* to it. Without the second half, `EXEC_STARTED`
/// carrying `u64::MAX` becomes the client's own watchdog interval, and a
/// hostile runner hangs the CLI forever with one frame.
pub const EXEC_MAX_SECS: u64 = 24 * 60 * 60;

/// The command exists and is running.
///
/// Always the first frame of an exec stream (E2B's `StartEvent`). "It spawned"
/// and "here is output" are different facts: the first gets a short handshake
/// clock that is cleared when it lands, and the run then lives under the long
/// one.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecStarted {
    /// What the worker will actually enforce, after clamping.
    pub timeout_secs: u64,
    /// Where it is running, for diagnosis.
    pub cwd: String,
}

/// How it ended, with everything a receipt needs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExitMsg {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub wall_ms: u64,
    pub cpu_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rss_kb: Option<i64>,
    /// What the runner's egress proxy saw. Observed outside the box by an h5i
    /// we authenticated. The `runner-observed` lane (design-runner.md R10).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub egress: Option<serde_json::Value>,
    /// True when output was cut at [`MAX_EXEC_OUTPUT`]. Said rather than
    /// silent: a truncated log that looks complete is worse than a short one.
    #[serde(default)]
    pub output_truncated: bool,
}

// ─── R13.4: export ───────────────────────────────────────────────────────────

/// The ref an export bundle carries its tip under.
pub const EXPORT_REF: &str = "refs/h5i/export-src";

/// Ask a box for what it has become.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportRequest {
    pub box_id: String,
}

impl ExportRequest {
    pub fn validated(&self) -> Result<(), ProtoError> {
        check_id("box id", &self.box_id)
    }
}

/// What the box has become, and how to get it.
///
/// The bundle is *thin*: it carries `base..tip` and needs the base, which
/// this side already has because it sent it. That keeps an export proportional
/// to the work done rather than to the repository's history. The create
/// direction cannot do the same, because the far side starts with nothing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportResult {
    pub box_id: String,
    /// The commit the worker made of the box's current state.
    pub tip_commit: String,
    /// Its tree. Carried so the receiving side can check that what it fetched
    /// is what was described before it trusts a byte of it.
    pub tip_tree: String,
    /// False when the box's tree is identical to its base: nothing was done in
    /// it. Said explicitly, because "an empty patch" and "the export failed"
    /// look the same from a distance.
    pub has_changes: bool,
    pub bytes: u64,
    pub sha256: String,
}

impl ExitMsg {
    /// The one reply that had no cleaning, and it carries the egress summary
    /// straight into a receipt.
    /// `EgressSummary.hosts[].host` is an arbitrary string from a machine the
    /// threat model says may be compromised, and the producer-side cap on how
    /// many there are binds the honest worker and not the other kind. Nothing
    /// renders those strings today, which is why this is consistency rather
    /// than a hole; a receipt is a durable record, and what goes into one
    /// should be bounded when it is written.
    pub fn sanitized(mut self) -> Result<Self, ProtoError> {
        if let Some(egress) = self.egress.take() {
            let text = serde_json::to_string(&egress).unwrap_or_default();
            if text.len() > MAX_EGRESS_JSON {
                return Err(ProtoError::Invalid(format!(
                    "the runner's egress summary is {} bytes, over the {MAX_EGRESS_JSON} \
                     this protocol accepts",
                    text.len()
                )));
            }
            self.egress = Some(egress);
        }
        Ok(self)
    }
}

/// How much egress evidence one exec may report. Generous for a real run,
/// bounded for one that is not.
pub const MAX_EGRESS_JSON: usize = 256 * 1024;

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
        let mut start = tail.len().saturating_sub(MAX_LOG_TAIL);
        // Slicing a `String` at a byte offset panics unless it is a character
        // boundary, and this offset is arithmetic on a length. A log whose
        // 16 KiB mark lands mid-character would take the process down. Walk
        // forward to the next boundary instead: losing up to three bytes off
        // the front of a log tail costs nothing.
        while start < tail.len() && !tail.is_char_boundary(start) {
            start += 1;
        }
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

/// The two policy fields of a [`CreateRequest`], from a resolved policy.
///
/// One function so the pair cannot drift: a digest computed over anything but
/// the value actually sent is a check that passes while meaning nothing. The
/// worker recomputes the same digest from what arrives, and only three fields
/// of a `ResolvedPolicy` serialise, none of them a host path, so the two
/// sides have to agree (design-runner.md R7).
pub fn policy_fields(
    policy: &h5i_sandbox::sandbox_policy::ResolvedPolicy,
) -> Result<(serde_json::Value, String), ProtoError> {
    let value = serde_json::to_value(policy).map_err(|source| ProtoError::Malformed {
        kind: "resolved policy",
        source,
    })?;
    let digest = policy
        .digest()
        .map_err(|e| ProtoError::Invalid(format!("could not digest the resolved policy: {e}")))?;
    Ok((value, digest))
}

impl CreateRequest {
    /// Refuse a request this worker must not act on.
    ///
    /// Every check here is about something that becomes a *path* or a
    /// *decision* on the runner. A peer-supplied identifier that becomes a
    /// directory name is the classic way a protocol grows a traversal bug, so
    /// the check is shape-based and total rather than a blocklist of things
    /// that looked dangerous when it was written.
    pub fn validated(&self) -> Result<(), ProtoError> {
        check_id("box id", &self.box_id)?;
        check_id("operation id", &self.operation_id)?;
        check_digest("request digest", &self.request_digest)?;
        check_digest("policy digest", &self.policy_digest)?;

        IsolationClaim::parse(&self.isolation).map_err(|e| {
            ProtoError::Invalid(format!("create names a tier h5i does not have: {e}"))
        })?;

        if let Some(image) = &self.image {
            // Not parsed as a reference here: what an image name means is the
            // container runtime's business. What this layer owes is that it is
            // a printable, bounded, single-line string.
            clean_field("image", image)?;
        }

        match self.source.kind {
            SourceKind::GitBundle => {
                check_digest("source digest", &self.source.sha256)?;
                if self.source.bytes == 0 {
                    return Err(ProtoError::Invalid(
                        "a git bundle source declares no bytes".into(),
                    ));
                }
                if self.source.bytes > MAX_SOURCE_BYTES {
                    return Err(ProtoError::Invalid(format!(
                        "source declares {} bytes, over the {MAX_SOURCE_BYTES} byte limit",
                        self.source.bytes
                    )));
                }
                if let Some(c) = &self.source.base_commit {
                    check_hex("base commit", c, 4, 64)?;
                }
            }
            SourceKind::Empty => {
                if self.source.bytes != 0 {
                    return Err(ProtoError::Invalid(
                        "an empty source declares bytes that will never be sent".into(),
                    ));
                }
            }
        }

        if self.lease.ttl_secs == 0 || self.lease.hard_ttl_secs == 0 {
            return Err(ProtoError::Invalid("a lease of zero never lives".into()));
        }
        if self.lease.ttl_secs > self.lease.hard_ttl_secs {
            return Err(ProtoError::Invalid(format!(
                "the idle lease ({}s) outlives the hard one ({}s), so the hard limit \
                 would never be reached",
                self.lease.ttl_secs, self.lease.hard_ttl_secs
            )));
        }
        Ok(())
    }
}

impl DestroyRequest {
    pub fn validated(&self) -> Result<(), ProtoError> {
        check_id("box id", &self.box_id)
    }
}

/// An identifier that becomes a path segment.
///
/// ASCII alphanumerics, `-`, `_` and `.`, never starting with `.` and never
/// containing a separator. That excludes `..`, absolute paths, and anything
/// with a `/` in it by construction rather than by a list of things to reject.
pub fn check_id(what: &'static str, value: &str) -> Result<(), ProtoError> {
    let ok = !value.is_empty()
        && value.len() <= MAX_ID
        && value.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.');
    if ok {
        Ok(())
    } else {
        Err(ProtoError::Invalid(format!(
            "{what} `{}` is not usable as a name on the runner — it becomes a directory, \
             so it takes letters, digits, `-`, `_` and `.`, starting with a letter or digit",
            // Truncated first. Echoing the whole value turns a one-megabyte
            // identifier into a one-megabyte refusal, which is a peer choosing
            // how much we allocate to complain about it.
            truncate(&sanitize_display(value), 64)
        )))
    }
}

fn check_digest(what: &'static str, value: &str) -> Result<(), ProtoError> {
    check_hex(what, value, DIGEST_HEX, DIGEST_HEX)
}

fn check_hex(what: &'static str, value: &str, min: usize, max: usize) -> Result<(), ProtoError> {
    if value.len() < min || value.len() > max || !value.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(ProtoError::Invalid(format!(
            "{what} is not {} hex characters",
            if min == max {
                min.to_string()
            } else {
                format!("{min} to {max}")
            }
        )));
    }
    Ok(())
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
/// the message, not later, in the middle of a create, as a mysterious unknown
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
    fn a_log_tail_cut_mid_character_does_not_panic() {
        // The cut is arithmetic on a byte length, so a log whose boundary lands
        // inside a multi-byte character would panic the process that built the
        // message. On the worker, while reporting somebody else's error.
        for pad in 0..8 {
            let long = "x".repeat(MAX_LOG_TAIL + pad) + "日本語のエラー";
            let e = ErrorMsg::new(ErrorCode::Internal, "boom").with_log_tail(long);
            let tail = e.log_tail.expect("a tail");
            assert!(tail.len() <= MAX_LOG_TAIL + 8);
            assert!(tail.ends_with("エラー"), "the end survives: {tail:?}");
        }
        // And the same for a cut that lands inside a character at the front.
        let long = "あ".repeat(MAX_LOG_TAIL);
        let e = ErrorMsg::new(ErrorCode::Internal, "boom").with_log_tail(long);
        assert!(e.log_tail.is_some());
    }

    #[test]
    fn a_log_tail_keeps_its_end_and_is_bounded() {
        // The tail is what matters (the failure is at the end of the log, not
        // the start) and it is the one peer-supplied string allowed to be big.
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

    fn create() -> CreateRequest {
        CreateRequest {
            box_id: "demo".into(),
            operation_id: "op1".into(),
            request_digest: "a".repeat(64),
            isolation: "container".into(),
            image: Some("docker.io/library/debian:bookworm".into()),
            limits: ResourceLimits::default(),
            lease: LeaseSpec::default(),
            policy_digest: "b".repeat(64),
            policy: serde_json::json!({"profile": "agent"}),
            source: SourceSpec {
                kind: SourceKind::GitBundle,
                bytes: 4096,
                sha256: "c".repeat(64),
                base_commit: Some("deadbeef".into()),
            },
        }
    }

    #[test]
    fn a_well_formed_create_validates() {
        create().validated().expect("valid");
    }

    #[test]
    fn an_identifier_can_never_escape_the_runners_box_directory() {
        // These become directory names on a machine we do not own. The check is
        // shape-based rather than a blocklist, so this is a sample of a closed
        // set rather than the set itself.
        for bad in [
            "", "..", "../../etc", "a/b", "a\\b", ".hidden", "-lead", "a b",
            "a\u{0}b", "boxes/../..",
        ] {
            let mut c = create();
            c.box_id = bad.to_string();
            assert!(
                c.validated().is_err(),
                "`{bad}` must not be usable as a box id"
            );
        }
        for good in ["demo", "pr-1234", "box_2", "a.b", "A1"] {
            let mut c = create();
            c.box_id = good.to_string();
            assert!(c.validated().is_ok(), "`{good}` should be allowed");
        }
    }

    #[test]
    fn an_overlong_identifier_is_refused() {
        let mut c = create();
        c.box_id = "a".repeat(MAX_ID + 1);
        assert!(c.validated().is_err());
    }

    #[test]
    fn a_digest_that_is_not_a_digest_is_refused() {
        // Every one of these would otherwise be compared against a real digest
        // and silently never match, which is a check that cannot fail loudly.
        for bad in ["", "abc", &"z".repeat(64), &"a".repeat(63), &"a".repeat(65)] {
            let mut c = create();
            c.request_digest = bad.to_string();
            assert!(c.validated().is_err(), "`{bad}` is not a digest");
        }
    }

    #[test]
    fn a_tier_the_product_does_not_have_is_refused_at_create() {
        let mut c = create();
        c.isolation = "quantum".into();
        assert!(matches!(c.validated(), Err(ProtoError::Invalid(_))));
    }

    #[test]
    fn a_source_declaring_more_than_the_cap_is_refused_before_a_byte_moves() {
        let mut c = create();
        c.source.bytes = MAX_SOURCE_BYTES + 1;
        assert!(c.validated().is_err());
    }

    #[test]
    fn the_two_source_kinds_must_agree_with_their_own_byte_counts() {
        // A bundle with no bytes would wait forever for data; an empty source
        // with bytes would leave a transfer nobody reads.
        let mut c = create();
        c.source.bytes = 0;
        assert!(c.validated().is_err(), "a bundle must carry bytes");

        let mut c = create();
        c.source = SourceSpec {
            kind: SourceKind::Empty,
            bytes: 0,
            sha256: String::new(),
            base_commit: None,
        };
        assert!(c.validated().is_ok(), "an empty source needs no digest");

        c.source.bytes = 10;
        assert!(c.validated().is_err(), "an empty source that declares bytes");
    }

    #[test]
    fn a_lease_that_could_never_expire_the_way_it_says_is_refused() {
        let mut c = create();
        c.lease = LeaseSpec {
            ttl_secs: 0,
            hard_ttl_secs: 10,
        };
        assert!(c.validated().is_err(), "a lease of zero never lives");

        let mut c = create();
        c.lease = LeaseSpec {
            ttl_secs: 100,
            hard_ttl_secs: 10,
        };
        assert!(
            c.validated().is_err(),
            "an idle lease outliving the hard one makes the hard limit unreachable"
        );
    }

    #[test]
    fn the_r13_2_messages_round_trip_as_json() {
        let bytes = encode(&create()).unwrap();
        let back: CreateRequest = decode("CREATE_BOX", &bytes).unwrap();
        assert_eq!(back.box_id, "demo");
        assert_eq!(back.source.kind, SourceKind::GitBundle);
        assert_eq!(back.policy["profile"], "agent");

        let result = CreateResult {
            box_id: "demo".into(),
            existing: true,
            policy_digest: "b".repeat(64),
            workspace: "/var/lib/h5i/runner/live/demo/work".into(),
            container: Some("h5i-demo".into()),
            lease_expires_at: chrono::Utc::now(),
        };
        let back: CreateResult = decode("CREATE_RESULT", &encode(&result).unwrap()).unwrap();
        assert!(back.existing);

        let list = ListResult {
            boxes: vec![BoxSummary {
                box_id: "demo".into(),
                state: BoxState::Live,
                isolation: "container".into(),
                created_at: chrono::Utc::now(),
                lease_expires_at: chrono::Utc::now(),
                container: None,
                policy_digest: "b".repeat(64),
            }],
        };
        let back: ListResult = decode("LIST", &encode(&list).unwrap()).unwrap();
        assert_eq!(back.boxes[0].state, BoxState::Live);
        assert_eq!(back.boxes[0].state.as_str(), "live");
    }

    #[test]
    fn a_destroy_names_a_box_that_could_exist() {
        let mut d = DestroyRequest {
            box_id: "demo".into(),
            force: false,
        };
        assert!(d.validated().is_ok());
        d.box_id = "../etc".into();
        assert!(d.validated().is_err());
    }

    #[test]
    fn a_hostile_runners_answers_are_refused_before_they_are_stored() {
        // Everything a runner says about itself is peer data. The same
        // category as a manifest pulled from someone else's clone. These are
        // the fields that reach a receipt and a terminal.
        let good = CreateResult {
            box_id: "demo".into(),
            existing: false,
            policy_digest: "a".repeat(64),
            workspace: "/var/lib/h5i/work".into(),
            container: None,
            lease_expires_at: chrono::Utc::now(),
        };
        assert!(good.clone().sanitized().is_ok());

        let mut evil = good.clone();
        evil.workspace = "/work\u{1b}[2J\u{1b}[H✔ applied".into();
        let cleaned = evil.sanitized().expect("cleaned, not refused");
        assert!(!cleaned.workspace.contains('\u{1b}'), "{:?}", cleaned.workspace);

        let mut evil = good.clone();
        evil.box_id = "../../etc/passwd".into();
        assert!(evil.sanitized().is_err(), "a box id is a path segment");

        let mut evil = good.clone();
        evil.policy_digest = "not a digest".into();
        assert!(evil.sanitized().is_err());

        // The exec cwd lands in a receipt that `box status` renders.
        let started = ExecStarted {
            timeout_secs: 30,
            cwd: "/work\u{1b}]0;title\u{7}".into(),
        };
        assert!(!started.sanitized().unwrap().cwd.contains('\u{1b}'));

        // A list is still a list this side prints.
        let mut list = ListResult {
            boxes: vec![BoxSummary {
                box_id: "ok".into(),
                state: BoxState::Live,
                isolation: "container\u{1b}[31m".into(),
                created_at: chrono::Utc::now(),
                lease_expires_at: chrono::Utc::now(),
                container: None,
                policy_digest: String::new(),
            }],
        };
        let cleaned = list.clone().sanitized().expect("cleaned");
        assert!(!cleaned.boxes[0].isolation.contains('\u{1b}'));

        list.boxes = vec![cleaned.boxes[0].clone(); MAX_LISTED_BOXES + 1];
        assert!(list.sanitized().is_err(), "a list of millions is fuzzing");

        // An export's object ids are compared against what arrives, and are
        // also printed; a value that is not an object id is refused cheaply.
        let export = ExportResult {
            box_id: "demo".into(),
            tip_commit: "z".repeat(40),
            tip_tree: "a".repeat(40),
            has_changes: true,
            bytes: 10,
            sha256: "a".repeat(64),
        };
        assert!(export.sanitized().is_err());
    }

    #[test]
    fn a_payload_that_is_not_json_is_malformed_not_a_panic() {
        let e = decode::<Hello>("HELLO", b"{not json").unwrap_err();
        assert_eq!(e.code(), ErrorCode::Malformed);
    }
}
