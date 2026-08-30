//! h5i-bpf — runtime detection: a kernel-observed evidence lane.
//!
//! Every other lane sits at the **boundary** of a box (h5i as parent, the
//! CONNECT proxy, the runner's channel) or **inside** it (the tee shim, the
//! browser), so both are defeated by work below the outermost command and by a
//! box that declines to cooperate. This lane is neither: the kernel reports
//! `execve`, `connect` and `openat` whether or not the box wanted them reported.
//!
//! ROADMAP D1 to D14 carry the design and the limits. Three belong here:
//!
//! * **It cannot deny anything.** No `bpf_send_signal`, no
//!   `bpf_override_return`, no LSM programs. Denial belongs to the mechanisms
//!   that fail closed by construction, and a second thing that sometimes blocks
//!   would blur a sharp boundary.
//! * **It reads no kernel structure.** Syscall tracepoint arguments and stable
//!   helpers only, so no CO-RE, no BTF, and one object that loads on every
//!   kernel from 5.8 up.
//! * **It never renders an absence as a clean result.** A run it could not watch
//!   carries [`RuntimeEvidence::unavailable`] with the reason; an incomplete one
//!   carries [`Coverage::Partial`]; a lossy one carries the drop count.
//!
//! [`probe`] asks what the host can do, [`event`] is the wire format, [`rules`]
//! is the pure signature engine and the only part that knows what a credential
//! is, [`evidence`] is what lands in a receipt, [`scope`] decides which events
//! belong to a box, and `session` (Linux, `load` feature) is the only module
//! that touches a kernel.
//!
//! ```no_run
//! use h5i_bpf::{DetectConfig, Watch, scope::Tier, rules::RuleContext};
//!
//! let cfg = DetectConfig {
//!     tier: Tier::Process,
//!     context: RuleContext { net_mode: "proxy".into(), ..Default::default() },
//!     ..DetectConfig::default()
//! };
//! let watch = Watch::start(cfg);   // never fails; refusals become evidence
//! // ... run the box ...
//! let block = watch.finish();      // goes straight into the receipt
//! ```

pub mod event;
pub mod evidence;
pub mod probe;
pub mod rules;
pub mod scope;

#[cfg(all(target_os = "linux", feature = "load"))]
mod session;

pub use evidence::{Coverage, Detection, RuntimeEvidence, Severity};
pub use probe::{BpfCaps, probe as probe_host};
pub use rules::{RULES, RuleContext, RuleSpec};
pub use scope::Tier;

/// Smallest ring buffer worth having. Below this a single `npm ci` loses
/// events faster than a reader can drain them, and a lane whose numbers are
/// always a lower bound teaches people to ignore the numbers.
pub const MIN_BUFFER_KB: u32 = 64;
/// Largest ring buffer a profile may ask for. The ceiling exists because the
/// memory is locked kernel memory, charged to h5i.
pub const MAX_BUFFER_KB: u32 = 16 * 1024;
/// What a profile gets when it does not say. Sized against a `cargo build`.
pub const DEFAULT_BUFFER_KB: u32 = 256;

/// Everything a run needs to be watched.
#[derive(Debug, Clone)]
pub struct DetectConfig {
    /// Which tier the run uses. Decides coverage, not behaviour.
    pub tier: Tier,
    /// Ring-buffer size, clamped to [`MIN_BUFFER_KB`]..=[`MAX_BUFFER_KB`].
    pub buffer_kb: u32,
    /// Rule selectors: ids, family names, or `*`.
    pub rules: Vec<String>,
    /// What the rules need to know about this box.
    pub context: RuleContext,
    /// Absolute path prefixes the kernel should not filter out. Built by
    /// [`kernel_prefixes`] from the same vocabulary the rules use, so the
    /// in-kernel filter and the userspace signatures can never drift into
    /// disagreeing about what is interesting.
    pub prefixes: Vec<String>,
    /// Ship every `openat`, including read-only opens that hit no prefix.
    /// Complete, and loud: a `cargo build` produces six figures of them.
    pub open_all: bool,
}

impl Default for DetectConfig {
    fn default() -> Self {
        Self {
            tier: Tier::Unknown,
            buffer_kb: DEFAULT_BUFFER_KB,
            rules: vec!["*".to_string()],
            context: RuleContext::default(),
            prefixes: Vec::new(),
            open_all: false,
        }
    }
}

impl DetectConfig {
    /// Resolve the rule selectors into the context, and report what did not
    /// resolve.
    ///
    /// Unknown selectors are returned rather than ignored. A profile that
    /// believes it enabled `net.direct-egres` and silently enabled nothing is
    /// exactly the class of quiet failure this lane exists to catch elsewhere,
    /// and it would be embarrassing to ship it here.
    pub fn resolve(&mut self) -> Vec<String> {
        let (on, unknown) = rules::select(&self.rules);
        self.context.enabled = on;
        unknown
    }

    /// Does any enabled rule need the in-kernel `.env` scan? Asked rather than
    /// assumed so the one filter that costs a path scan is not paid for by
    /// runs whose rule set does not want it.
    pub fn want_dotenv(&self) -> bool {
        self.context.enabled.is_empty() || self.context.enabled.contains("secret.dotenv")
    }
}

/// The absolute prefixes the kernel filter should let through, derived from
/// the rules' own vocabulary.
///
/// Bounded by [`event::MAX_PREFIX`], and ordered by value so that a truncation
/// drops the least important entry rather than an arbitrary one.
///
/// `home` and `control_dir` are the box's paths, not the host's — a box has
/// its own home, and a filter built from the host's would match nothing.
pub fn kernel_prefixes(home: &str, control_dir: &str) -> Vec<String> {
    // Most valuable first, because the tail is what a truncation drops.
    let mut out = Vec::new();
    // `/proc/` is broad, and it is the only way to see a read of another
    // process's environment. The rules narrow it back down to
    // `/proc/<pid>/environ` for a pid outside the box.
    out.push("/proc/".to_string());
    let control = control_dir.trim_end_matches('/');
    if !control.is_empty() {
        out.push(control.to_string());
    }
    let home = home.trim_end_matches('/');
    if !home.is_empty() {
        for rel in [
            ".ssh",
            ".aws",
            ".config/gh",
            ".git-credentials",
            ".netrc",
            ".docker",
            ".kube",
            ".gnupg",
            ".npmrc",
            ".pypirc",
            ".config/gcloud",
            ".config/h5i",
        ] {
            out.push(format!("{home}/{rel}"));
        }
    }
    out.truncate(event::MAX_PREFIX);
    out
}

/// A run being watched, or the reason it is not.
///
/// One type for both outcomes on purpose. A caller that had to handle "started
/// a session" and "could not start a session" differently would eventually
/// handle the second one by logging it, and the receipt would carry nothing —
/// which is the failure mode this lane is a correction for.
pub struct Watch(WatchInner);

enum WatchInner {
    #[cfg(all(target_os = "linux", feature = "load"))]
    Live(Box<session::Session>),
    Off(Box<RuntimeEvidence>),
}

impl Watch {
    /// Start watching. Never fails: a refusal becomes the evidence.
    pub fn start(mut cfg: DetectConfig) -> Self {
        let unknown = cfg.resolve();
        if !unknown.is_empty() {
            return Watch::off(
                cfg.tier,
                format!(
                    "the profile names {} no rule provides: {}",
                    if unknown.len() == 1 { "a rule" } else { "rules" },
                    unknown.join(", ")
                ),
            );
        }

        let caps = probe::probe();
        if let Some(why) = caps.unavailable_reason() {
            // "fix", not "grant": the command is sometimes a capability grant
            // and sometimes a rebuild, and one word that covers both is better
            // than one that is wrong half the time.
            let why = match caps.fix {
                Some(fix) => format!("{why} — fix: {fix}"),
                None => why,
            };
            return Watch::off(cfg.tier, why);
        }

        #[cfg(all(target_os = "linux", feature = "load"))]
        {
            match session::Session::start(&cfg) {
                Ok(s) => Watch(WatchInner::Live(Box::new(s))),
                Err(why) => Watch::off(cfg.tier, why),
            }
        }
        #[cfg(not(all(target_os = "linux", feature = "load")))]
        {
            Watch::off(
                cfg.tier,
                "this build was compiled without the runtime-detection collector".to_string(),
            )
        }
    }

    fn off(tier: Tier, why: String) -> Self {
        #[cfg(all(target_os = "linux", feature = "load"))]
        {
            Watch(WatchInner::Off(Box::new(session::refused(tier, why))))
        }
        #[cfg(not(all(target_os = "linux", feature = "load")))]
        {
            let mut ev = RuntimeEvidence::unavailable(why);
            if let (Coverage::None, Some(reason)) = tier.coverage() {
                ev.coverage_reason = Some(reason.to_string());
            }
            Watch(WatchInner::Off(Box::new(ev)))
        }
    }

    /// Is a probe actually attached? For a `--json` status line, and for the
    /// `require = true` check, which refuses to run rather than proceed
    /// unwatched.
    pub fn is_live(&self) -> bool {
        match &self.0 {
            #[cfg(all(target_os = "linux", feature = "load"))]
            WatchInner::Live(_) => true,
            WatchInner::Off(_) => false,
        }
    }

    /// Why it is not live. `None` when it is.
    pub fn refusal(&self) -> Option<&str> {
        match &self.0 {
            #[cfg(all(target_os = "linux", feature = "load"))]
            WatchInner::Live(_) => None,
            WatchInner::Off(ev) => ev.unavailable.as_deref(),
        }
    }

    /// Stop, and produce the block the receipt carries.
    pub fn finish(self) -> RuntimeEvidence {
        match self.0 {
            #[cfg(all(target_os = "linux", feature = "load"))]
            WatchInner::Live(s) => s.finish(),
            WatchInner::Off(ev) => *ev,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes_are_bounded_and_absolute() {
        let p = kernel_prefixes("/home/box", "/home/box/work/.h5i");
        assert!(p.len() <= event::MAX_PREFIX);
        assert!(p.iter().all(|s| s.starts_with('/')));
        assert!(p.iter().any(|s| s.ends_with("/.ssh")));
        assert!(p.contains(&"/proc/".to_string()));
        assert!(p.contains(&"/home/box/work/.h5i".to_string()));
    }

    /// Every prefix must fit the kernel's fixed-size slot. One that does not
    /// is dropped by the loader, so an over-long one here would silently
    /// disable a filter.
    #[test]
    fn every_prefix_fits_the_kernel_slot() {
        let p = kernel_prefixes("/home/a-fairly-long-username-here", "/home/x/.h5i/env/a/b/work");
        for s in &p {
            assert!(s.len() <= event::PREFIX_LEN, "{s} is {} bytes", s.len());
        }
    }

    /// An empty home must not produce `/.ssh`, which is a real path and not
    /// the one anybody meant.
    #[test]
    fn an_unknown_home_contributes_no_prefixes() {
        let p = kernel_prefixes("", "");
        assert_eq!(p, vec!["/proc/".to_string()]);
    }

    #[test]
    fn a_trailing_slash_does_not_double_up() {
        let p = kernel_prefixes("/home/box/", "/work/.h5i/");
        assert!(p.contains(&"/home/box/.ssh".to_string()));
        assert!(p.contains(&"/work/.h5i".to_string()));
    }

    #[test]
    fn selectors_resolve_into_the_context() {
        let mut cfg = DetectConfig {
            rules: vec!["net".into(), "kernel.bpf".into()],
            ..Default::default()
        };
        assert!(cfg.resolve().is_empty());
        assert!(cfg.context.enabled.contains("net.direct-egress"));
        assert!(cfg.context.enabled.contains("kernel.bpf"));
        assert!(!cfg.context.enabled.contains("secret.read"));
        assert!(!cfg.want_dotenv());
    }

    /// A typo in a profile must stop the run being reported as watched. The
    /// alternative is a box that quietly has one fewer rule than its policy
    /// says.
    #[test]
    fn an_unknown_selector_refuses_rather_than_silently_narrowing() {
        let cfg = DetectConfig {
            rules: vec!["nte.direct-egress".into()],
            ..Default::default()
        };
        let w = Watch::start(cfg);
        assert!(!w.is_live());
        let why = w.refusal().unwrap().to_string();
        assert!(why.contains("nte.direct-egress"), "{why}");
        let ev = w.finish();
        assert!(!ev.observed());
    }

    /// `Watch::start` is on the run path. It must never panic and must always
    /// produce a block, whatever the host is.
    #[test]
    fn starting_always_produces_a_block() {
        let w = Watch::start(DetectConfig {
            tier: Tier::Workspace,
            ..Default::default()
        });
        let live = w.is_live();
        let ev = w.finish();
        assert_eq!(ev.lane, evidence::LANE);
        if !live {
            assert!(ev.unavailable.is_some(), "an unwatched run must say why");
            assert!(!ev.observed());
        }
    }

    /// The microVM tier's reason has to survive even when the refusal is about
    /// something else entirely: "you lack CAP_BPF" would otherwise imply that
    /// granting it would help, and on a guest kernel it would not.
    #[test]
    fn the_microvm_reason_survives_an_unrelated_refusal() {
        let w = Watch::start(DetectConfig {
            tier: Tier::Microvm,
            rules: vec!["no.such-rule".into()],
            ..Default::default()
        });
        let ev = w.finish();
        assert_eq!(ev.coverage, Coverage::None);
        assert!(
            ev.coverage_reason.as_deref().unwrap_or("").contains("guest"),
            "{:?}",
            ev.coverage_reason
        );
    }

    /// The collector rounds a requested buffer up to a power of two, so a
    /// default that is not one would silently become a different number than
    /// the one documented.
    #[test]
    fn a_requested_buffer_is_clamped_into_the_documented_range() {
        let clamp = |kb: u32| kb.clamp(MIN_BUFFER_KB, MAX_BUFFER_KB).next_power_of_two();
        assert_eq!(clamp(DEFAULT_BUFFER_KB), DEFAULT_BUFFER_KB);
        assert_eq!(clamp(0), MIN_BUFFER_KB);
        assert_eq!(clamp(u32::MAX), MAX_BUFFER_KB);
        assert_eq!(clamp(100), 128);
    }
}
