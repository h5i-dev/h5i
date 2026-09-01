//! Loading the probe, attaching it, and reading what comes back.

use std::os::fd::AsRawFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use aya::maps::{Array, HashMap as BpfHashMap, MapData, PerCpuArray, RingBuf};
use aya::programs::TracePoint;
use aya::{Ebpf, EbpfLoader};

use crate::event::{self, Event, EventKind, MAX_PREFIX, PREFIX_LEN};
use crate::evidence::{Coverage, LANE, RuntimeEvidence};
use crate::rules::Engine;
use crate::scope::{self, SCOPE_PIDTREE, Tier};
use crate::{DetectConfig, MAX_BUFFER_KB, MIN_BUFFER_KB};

/// The probe object, compiled by `build.rs` from `bpf/h5i_detect.bpf.c`.
///
/// `include_bytes!` of a build-script output rather than a file read at run
/// time: the object and the loader are one artifact, and a binary that could
/// be pointed at a different probe object would be a binary whose evidence
/// lane can be replaced by anyone who can write a file.
#[cfg(h5i_bpf_object)]
const PROBE_OBJECT: &[u8] = include_bytes!(env!("H5I_BPF_OBJECT"));

/// `struct h5i_config` in `bpf/h5i_event.h`.
#[repr(C)]
#[derive(Clone, Copy)]
struct ConfigWire {
    kind_mask: u64,
    prefix_count: u32,
    open_all: u32,
    want_dotenv: u32,
    _pad: u32,
}
unsafe impl aya::Pod for ConfigWire {}
const _: () = assert!(std::mem::size_of::<ConfigWire>() == 24);

/// `struct h5i_prefix` in `bpf/h5i_event.h`.
#[repr(C)]
#[derive(Clone, Copy)]
struct PrefixWire {
    len: u32,
    s: [u8; PREFIX_LEN],
}
unsafe impl aya::Pod for PrefixWire {}
const _: () = assert!(std::mem::size_of::<PrefixWire>() == 68);

/// Every program in the object, with the tracepoint it attaches to.
///
/// The table is the single place the two names live. A program in the object
/// with no row here would silently never attach, so [`Session::start`] fails
/// if the object holds a `tracepoint/` program this table does not mention.
const PROGRAMS: &[(&str, &str, &str)] = &[
    ("h5i_sys_enter_execve", "syscalls", "sys_enter_execve"),
    ("h5i_sys_enter_execveat", "syscalls", "sys_enter_execveat"),
    ("h5i_sys_enter_openat", "syscalls", "sys_enter_openat"),
    ("h5i_sys_enter_openat2", "syscalls", "sys_enter_openat2"),
    ("h5i_sys_enter_connect", "syscalls", "sys_enter_connect"),
    ("h5i_sys_enter_socket", "syscalls", "sys_enter_socket"),
    ("h5i_sys_enter_ptrace", "syscalls", "sys_enter_ptrace"),
    ("h5i_sys_enter_bpf", "syscalls", "sys_enter_bpf"),
    ("h5i_sys_enter_unshare", "syscalls", "sys_enter_unshare"),
    ("h5i_sys_enter_setns", "syscalls", "sys_enter_setns"),
    ("h5i_sys_enter_init_module", "syscalls", "sys_enter_init_module"),
    ("h5i_sys_enter_finit_module", "syscalls", "sys_enter_finit_module"),
    ("h5i_sys_enter_memfd_create", "syscalls", "sys_enter_memfd_create"),
    ("h5i_sys_enter_mount", "syscalls", "sys_enter_mount"),
    ("h5i_sys_enter_pivot_root", "syscalls", "sys_enter_pivot_root"),
    ("h5i_sched_process_fork", "sched", "sched_process_fork"),
    ("h5i_sched_process_exit", "sched", "sched_process_exit"),
];

/// Tracepoints whose *scheduler* field offsets the probe hardcodes, and what
/// it believes they are. Checked against the kernel's own `format` file when
/// tracefs is readable (design-detect.md D5).
type FieldLayout = (&'static str, usize, usize);

const SCHED_FIELDS: &[(&str, &[FieldLayout])] = &[
    (
        "sched_process_fork",
        &[("parent_pid", 24, 4), ("child_pid", 44, 4)],
    ),
    ("sched_process_exit", &[("pid", 24, 4)]),
];

/// How long the reader thread waits on the ring-buffer fd before checking
/// whether the run has ended. Short enough that stopping is prompt, long
/// enough that an idle run costs nothing.
const POLL_MS: libc::c_int = 100;

/// A live collector, for the length of one run.
pub struct Session {
    /// Holds the loaded programs. Dropping it detaches every one of them,
    /// which is how a session leaves nothing behind.
    ebpf: Ebpf,
    reader: Option<JoinHandle<Collected>>,
    stop: Arc<AtomicBool>,
    tier: Tier,
}

/// What the reader thread hands back when the run ends.
struct Collected {
    engine: Engine,
    /// Ring-buffer records the decoder refused, and the first reason.
    ///
    /// A nonzero count is a probe/loader mismatch (a bug in h5i, not
    /// behaviour of the box) and it goes into the receipt rather than into a
    /// log nobody reads, because a run whose evidence was silently thinner
    /// than it should have been is precisely the thing this lane exists to
    /// stop happening.
    rejected: u64,
    first_rejection: Option<String>,
}

impl Session {
    /// Load, program the scope, and attach. Returns the refusal as a string on
    /// any failure. The caller turns that into an `unavailable` block, so a
    /// failure to start is recorded rather than logged and forgotten.
    pub fn start(cfg: &DetectConfig) -> Result<Self, String> {
        #[cfg(not(h5i_bpf_object))]
        {
            let _ = cfg;
            Err("this build carries no eBPF probe object".to_string())
        }
        #[cfg(h5i_bpf_object)]
        {
            Self::start_inner(cfg)
        }
    }

    #[cfg(h5i_bpf_object)]
    fn start_inner(cfg: &DetectConfig) -> Result<Self, String> {
        raise_memlock();

        let buffer_bytes = cfg
            .buffer_kb
            .clamp(MIN_BUFFER_KB, MAX_BUFFER_KB)
            .next_power_of_two()
            .saturating_mul(1024);

        let mut ebpf = EbpfLoader::new()
            .map_max_entries("H5I_EVENTS", buffer_bytes)
            .load(PROBE_OBJECT)
            .map_err(|e| format!("loading the probe failed: {e}"))?;

        verify_tracepoint_layout()?;
        check_every_program_is_attached(&ebpf)?;
        program_maps(&mut ebpf, cfg)?;
        attach_all(&mut ebpf)?;

        // Taken last, after everything that can fail: the ring buffer is what
        // the reader thread owns, and a thread started before a failing attach
        // would have to be torn down again.
        let ring = ebpf
            .take_map("H5I_EVENTS")
            .ok_or_else(|| "the probe object has no H5I_EVENTS map".to_string())?;
        let ring: RingBuf<MapData> =
            RingBuf::try_from(ring).map_err(|e| format!("H5I_EVENTS is not a ring buffer: {e}"))?;

        let stop = Arc::new(AtomicBool::new(false));
        let engine = Engine::new(cfg.context.clone());
        let reader = spawn_reader(ring, engine, Arc::clone(&stop));

        Ok(Session {
            ebpf,
            reader: Some(reader),
            stop,
            tier: cfg.tier,
        })
    }

    /// Stop collecting and produce the block for the receipt.
    ///
    /// Never fails. A reader thread that cannot be joined, a stats map that
    /// cannot be read, a panic in the fold: each degrades one field of the
    /// answer, and every one of them is reported in the block rather than
    /// turned into an error that loses the whole run's evidence.
    pub fn finish(mut self) -> RuntimeEvidence {
        self.stop.store(true, Ordering::SeqCst);

        let (detections, seen, mut notes) = match self.reader.take().map(|h| h.join()) {
            Some(Ok(c)) => {
                let seen = c.engine.events_seen();
                let mut notes = Vec::new();
                if c.rejected > 0 {
                    notes.push(format!(
                        "{} ring-buffer record{} could not be decoded ({}) — this is a \
                         probe/loader mismatch in h5i, not box behaviour",
                        c.rejected,
                        if c.rejected == 1 { "" } else { "s" },
                        c.first_rejection.as_deref().unwrap_or("no reason recorded")
                    ));
                }
                (c.engine.finish(), seen, notes)
            }
            Some(Err(_)) => (
                Vec::new(),
                0,
                vec!["the collector thread panicked; no detections from this run".to_string()],
            ),
            None => (Vec::new(), 0, Vec::new()),
        };

        let (lost, filtered) = self.read_stats();
        let (coverage, tier_reason) = self.tier.coverage();

        let mut reason = tier_reason.map(|s| s.to_string());
        for extra in notes.drain(..) {
            reason = Some(match reason {
                Some(r) => format!("{r}; {extra}"),
                None => extra,
            });
        }

        RuntimeEvidence {
            lane: LANE.to_string(),
            scope: SCOPE_PIDTREE.to_string(),
            coverage,
            coverage_reason: reason,
            events_seen: seen,
            events_lost: lost,
            events_filtered: filtered,
            detections,
            unavailable: None,
        }
    }

    /// `(lost, filtered)`, summed across CPUs. Zero when the map cannot be
    /// read, which is indistinguishable from "nothing was lost" and is the one
    /// place this file has to accept that; the map is created by the same load
    /// that created everything else, so its absence would mean a much louder
    /// failure has already happened.
    fn read_stats(&self) -> (u64, u64) {
        let Some(map) = self.ebpf.map("H5I_STATS") else {
            return (0, 0);
        };
        let Ok(stats) = PerCpuArray::<_, u64>::try_from(map) else {
            return (0, 0);
        };
        let sum = |slot: u32| -> u64 {
            stats
                .get(&slot, 0)
                .map(|per_cpu| per_cpu.iter().copied().sum())
                .unwrap_or(0)
        };
        (sum(event::STAT_LOST), sum(event::STAT_FILTERED))
    }
}

impl Drop for Session {
    /// A session that is dropped without `finish` must still stop its thread,
    /// or a run that failed part way through leaves a thread polling a ring
    /// buffer whose maps are about to be unmapped.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.reader.take() {
            let _ = h.join();
        }
    }
}

/// Read events until asked to stop, then drain what is left.
///
/// The rules run on this thread, folded in as each event arrives, rather
/// than being shipped over a channel to be folded later. That removes a whole
/// category of loss, a full channel, and there is nothing to gain from the
/// extra hop: the fold is a few comparisons per event and the alternative is a
/// second place for events to disappear without being counted.
fn spawn_reader(
    mut ring: RingBuf<MapData>,
    engine: Engine,
    stop: Arc<AtomicBool>,
) -> JoinHandle<Collected> {
    std::thread::spawn(move || {
        let fd = ring.as_raw_fd();
        let mut out = Collected {
            engine,
            rejected: 0,
            first_rejection: None,
        };
        while !stop.load(Ordering::SeqCst) {
            drain(&mut ring, &mut out);
            wait_readable(fd, POLL_MS);
        }
        // The run has ended, but the kernel may have submitted records between
        // the last drain and the stop. Losing those would mean the last thing
        // a box did, often the interesting thing, is the one event that
        // never reaches the receipt.
        drain(&mut ring, &mut out);
        out
    })
}

fn drain(ring: &mut RingBuf<MapData>, out: &mut Collected) {
    while let Some(item) = ring.next() {
        match Event::decode(&item) {
            Ok(mut ev) => {
                // The probe cannot report a parent (design-detect.md D5); the
                // fold has been watching forks and can.
                if ev.kind != EventKind::Fork
                    && let Some(p) = out.engine.parent_of(ev.tid)
                {
                    ev.ppid = p;
                }
                out.engine.observe(&ev);
            }
            // A record we cannot parse is a probe/loader mismatch, which the
            // magic and version checks exist to make loud. Counted and
            // reported rather than skipped in silence.
            Err(e) => {
                out.rejected += 1;
                if out.first_rejection.is_none() {
                    out.first_rejection = Some(e.to_string());
                }
            }
        }
    }
}

/// `poll(2)` the ring-buffer fd. A timeout is not an error: it is how the loop
/// gets to check whether the run has ended.
fn wait_readable(fd: std::os::fd::RawFd, timeout_ms: libc::c_int) {
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: `pfd` is a valid, initialised pollfd for the duration of the
    // call, and the count matches the single element.
    unsafe {
        libc::poll(&mut pfd, 1, timeout_ms);
    }
}

/// Kernels before 5.11 charge BPF maps against `RLIMIT_MEMLOCK`, whose default
/// is 64 KiB. Smaller than the ring buffer. Best effort: on 5.11 and later
/// the limit is irrelevant, and where it is not, failing to raise it produces
/// a clear "load failed" a few lines below rather than a mystery here.
fn raise_memlock() {
    let lim = libc::rlimit {
        rlim_cur: libc::RLIM_INFINITY,
        rlim_max: libc::RLIM_INFINITY,
    };
    // SAFETY: a well-formed rlimit for a resource this process may raise when
    // it has the capability, and which fails harmlessly when it does not.
    unsafe {
        libc::setrlimit(libc::RLIMIT_MEMLOCK, &lim);
    }
}

/// Fail if the object holds a tracepoint program [`PROGRAMS`] does not name.
///
/// The failure this prevents is quiet: a program added to the probe and not to
/// the table loads, never attaches, and produces exactly the same receipt as a
/// box that did not do the thing it was watching for.
fn check_every_program_is_attached(ebpf: &Ebpf) -> Result<(), String> {
    let missing: Vec<String> = ebpf
        .programs()
        .map(|(name, _)| name.to_string())
        .filter(|name| !PROGRAMS.iter().any(|(p, _, _)| p == name))
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "the probe object has programs the loader does not attach: {}",
            missing.join(", ")
        ))
    }
}

fn program_maps(ebpf: &mut Ebpf, cfg: &DetectConfig) -> Result<(), String> {
    // The kind mask: every kind this build knows. Kinds are not individually
    // selectable from a profile on purpose. A rule needs the events it needs,
    // and letting a profile disable an event kind would let it disable a rule
    // without saying so.
    let kind_mask = EventKind::ALL
        .iter()
        .fold(0u64, |m, k| m | (1u64 << k.to_wire()));

    let prefixes: Vec<&String> = cfg.prefixes.iter().take(MAX_PREFIX).collect();

    {
        let map = ebpf
            .map_mut("H5I_CONFIG")
            .ok_or_else(|| "the probe object has no H5I_CONFIG map".to_string())?;
        let mut conf: Array<_, ConfigWire> =
            Array::try_from(map).map_err(|e| format!("H5I_CONFIG is not an array: {e}"))?;
        conf.set(
            0,
            ConfigWire {
                kind_mask,
                prefix_count: u32::try_from(prefixes.len()).unwrap_or(0),
                open_all: u32::from(cfg.open_all),
                want_dotenv: u32::from(cfg.want_dotenv()),
                _pad: 0,
            },
            0,
        )
        .map_err(|e| format!("writing H5I_CONFIG failed: {e}"))?;
    }

    {
        let map = ebpf
            .map_mut("H5I_PREFIXES")
            .ok_or_else(|| "the probe object has no H5I_PREFIXES map".to_string())?;
        let mut arr: Array<_, PrefixWire> =
            Array::try_from(map).map_err(|e| format!("H5I_PREFIXES is not an array: {e}"))?;
        for (i, p) in prefixes.iter().enumerate() {
            let bytes = p.as_bytes();
            // A prefix too long to hold is dropped rather than truncated: a
            // truncated prefix matches more than it was meant to, which turns
            // a filter into a flood.
            if bytes.is_empty() || bytes.len() > PREFIX_LEN {
                continue;
            }
            let mut w = PrefixWire {
                len: bytes.len() as u32,
                s: [0u8; PREFIX_LEN],
            };
            w.s[..bytes.len()].copy_from_slice(bytes);
            arr.set(i as u32, w, 0)
                .map_err(|e| format!("writing H5I_PREFIXES[{i}] failed: {e}"))?;
        }
    }

    {
        let map = ebpf
            .map_mut("H5I_TRACKED")
            .ok_or_else(|| "the probe object has no H5I_TRACKED map".to_string())?;
        let mut tracked: BpfHashMap<_, u32, u8> =
            BpfHashMap::try_from(map).map_err(|e| format!("H5I_TRACKED is not a hash: {e}"))?;
        // `0` is H5I_ST_SELF: these are h5i's own threads, so nothing they do
        // is emitted, but anything they fork is a candidate.
        for tid in scope::self_tids() {
            tracked
                .insert(tid, 0u8, 0)
                .map_err(|e| format!("seeding the process tree failed: {e}"))?;
        }
    }

    Ok(())
}

fn attach_all(ebpf: &mut Ebpf) -> Result<(), String> {
    for (prog, category, name) in PROGRAMS {
        let program = ebpf
            .program_mut(prog)
            .ok_or_else(|| format!("the probe object has no program {prog}"))?;
        let tp: &mut TracePoint = program
            .try_into()
            .map_err(|e| format!("{prog} is not a tracepoint program: {e}"))?;
        tp.load()
            .map_err(|e| format!("the verifier rejected {prog}: {e}"))?;
        tp.attach(category, name)
            .map_err(|e| format!("attaching {prog} to {category}:{name} failed: {e}"))?;
    }
    Ok(())
}

/// Hold the kernel to the field offsets the probe assumes.
/// The syscall-entry layout is fixed ABI and the scheduler tracepoints publish
/// theirs, so this is a check rather than a discovery. Best effort in one
/// direction only: tracefs is usually root-only, and an unreadable `format`
/// file leaves the assumption unverified and the load proceeding, but a
/// `format` file that is readable and disagrees is a hard refusal, because
/// silently reading the wrong four bytes of a fork event is how a scope quietly
/// stops tracking anything.
fn verify_tracepoint_layout() -> Result<(), String> {
    for (name, fields) in SCHED_FIELDS {
        let Some(text) = read_format("sched", name) else {
            continue;
        };
        for (field, offset, size) in *fields {
            match parse_field(&text, field) {
                Some((o, s)) if o == *offset && s == *size => {}
                Some((o, s)) => {
                    return Err(format!(
                        "this kernel's sched:{name} has {field} at offset {o} size {s}; the probe \
                         reads offset {offset} size {size}. Refusing to attach rather than \
                         misread it."
                    ));
                }
                None => {
                    return Err(format!(
                        "this kernel's sched:{name} has no {field} field; the probe cannot be \
                         trusted against it"
                    ));
                }
            }
        }
    }

    // The syscall-entry contexts share one layout, so one of them is a
    // sufficient check for all of them.
    if let Some(text) = read_format("syscalls", "sys_enter_openat") {
        match parse_field(&text, "__syscall_nr") {
            Some((8, 4)) => {}
            Some((o, s)) => {
                return Err(format!(
                    "this kernel's syscall-entry tracepoint has __syscall_nr at offset {o} size \
                     {s}; the probe reads offset 8. Refusing to attach."
                ));
            }
            None => {}
        }
        match parse_field(&text, "filename") {
            // `filename` is the second syscall argument of `openat`, so it
            // sits one register past the first, which starts at 16.
            Some((24, 8)) | None => {}
            Some((o, s)) => {
                return Err(format!(
                    "this kernel's syscalls:sys_enter_openat has filename at offset {o} size {s}; \
                     the probe reads the argument array from offset 16. Refusing to attach."
                ));
            }
        }
    }
    Ok(())
}

fn read_format(category: &str, name: &str) -> Option<String> {
    for base in ["/sys/kernel/tracing", "/sys/kernel/debug/tracing"] {
        let path = format!("{base}/events/{category}/{name}/format");
        if let Ok(text) = std::fs::read_to_string(&path) {
            return Some(text);
        }
    }
    None
}

/// Pull `(offset, size)` for one field out of a tracepoint `format` file.
///
/// Lines look like this, with tabs between the parts:
///
/// ```text
/// field:const char * filename;  offset:24;  size:8;  signed:0;
/// ```
fn parse_field(text: &str, field: &str) -> Option<(usize, usize)> {
    for line in text.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("field:") else {
            continue;
        };
        let (decl, tail) = rest.split_once(';')?;
        // The name is the last identifier in the declaration, minus any array
        // suffix: `char parent_comm[16]` names `parent_comm`.
        let name = decl
            .trim()
            .rsplit(|c: char| c.is_whitespace() || c == '*')
            .next()?
            .split('[')
            .next()?;
        if name != field {
            continue;
        }
        let value = |key: &str| -> Option<usize> {
            tail.split(';').find_map(|part| {
                part.trim()
                    .strip_prefix(key)?
                    .trim()
                    .parse::<usize>()
                    .ok()
            })
        };
        return Some((value("offset:")?, value("size:")?));
    }
    None
}

/// The block for a run whose collector never started. Kept here so the wording
/// lives beside the code that produces the successful one.
pub(crate) fn refused(tier: Tier, why: String) -> RuntimeEvidence {
    let mut ev = RuntimeEvidence::unavailable(why);
    // Coverage stays `None`, nothing was observed, but the tier's own reason
    // is still worth carrying: on the microVM tier the honest answer is that
    // no capability would have helped.
    if let (Coverage::None, Some(reason)) = tier.coverage() {
        ev.coverage_reason = Some(reason.to_string());
    }
    ev
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real `format` shape, from a live kernel.
    const OPENAT_FORMAT: &str = "name: sys_enter_openat\nID: 634\nformat:\n\
        \tfield:unsigned short common_type;\toffset:0;\tsize:2;\tsigned:0;\n\
        \tfield:unsigned char common_flags;\toffset:2;\tsize:1;\tsigned:0;\n\
        \tfield:unsigned char common_preempt_count;\toffset:3;\tsize:1;\tsigned:0;\n\
        \tfield:int common_pid;\toffset:4;\tsize:4;\tsigned:1;\n\
        \n\
        \tfield:int __syscall_nr;\toffset:8;\tsize:4;\tsigned:1;\n\
        \tfield:int dfd;\toffset:16;\tsize:8;\tsigned:0;\n\
        \tfield:const char * filename;\toffset:24;\tsize:8;\tsigned:0;\n\
        \tfield:int flags;\toffset:32;\tsize:8;\tsigned:0;\n";

    const FORK_FORMAT: &str = "name: sched_process_fork\nformat:\n\
        \tfield:unsigned short common_type;\toffset:0;\tsize:2;\tsigned:0;\n\
        \tfield:char parent_comm[16];\toffset:8;\tsize:16;\tsigned:0;\n\
        \tfield:pid_t parent_pid;\toffset:24;\tsize:4;\tsigned:1;\n\
        \tfield:char child_comm[16];\toffset:28;\tsize:16;\tsigned:0;\n\
        \tfield:pid_t child_pid;\toffset:44;\tsize:4;\tsigned:1;\n";

    #[test]
    fn field_offsets_parse_out_of_a_real_format_file() {
        assert_eq!(parse_field(OPENAT_FORMAT, "__syscall_nr"), Some((8, 4)));
        assert_eq!(parse_field(OPENAT_FORMAT, "filename"), Some((24, 8)));
        assert_eq!(parse_field(OPENAT_FORMAT, "dfd"), Some((16, 8)));
        assert_eq!(parse_field(OPENAT_FORMAT, "nonesuch"), None);
    }

    /// An array field's name has the `[16]` on it. Getting this wrong would
    /// make the fork check silently unverifiable. The failure mode the check
    /// exists to prevent.
    #[test]
    fn array_fields_parse_by_name_without_their_bounds() {
        assert_eq!(parse_field(FORK_FORMAT, "parent_comm"), Some((8, 16)));
        assert_eq!(parse_field(FORK_FORMAT, "parent_pid"), Some((24, 4)));
        assert_eq!(parse_field(FORK_FORMAT, "child_pid"), Some((44, 4)));
    }

    /// The offsets in [`SCHED_FIELDS`] are what the probe's structs encode.
    /// This is the check that would fail if somebody edited one and not the
    /// other.
    #[test]
    fn the_probes_assumed_offsets_match_a_real_kernels() {
        for (name, fields) in SCHED_FIELDS {
            let text = match *name {
                "sched_process_fork" => FORK_FORMAT,
                _ => continue,
            };
            for (field, offset, size) in *fields {
                assert_eq!(parse_field(text, field), Some((*offset, *size)), "{field}");
            }
        }
    }

    #[test]
    fn the_wire_structs_are_the_size_the_probe_expects() {
        assert_eq!(std::mem::size_of::<ConfigWire>(), 24);
        assert_eq!(std::mem::size_of::<PrefixWire>(), 4 + PREFIX_LEN);
    }

    /// Every program in the C file must have a row in [`PROGRAMS`]. The
    /// runtime check catches this too, but only on a host that can load; this
    /// one catches it in CI.
    #[test]
    fn every_program_in_the_probe_is_in_the_attach_table() {
        let src = include_str!("../bpf/h5i_detect.bpf.c");
        let mut declared = Vec::new();
        let mut lines = src.lines().peekable();
        while let Some(line) = lines.next() {
            let Some(rest) = line.trim().strip_prefix("SEC(\"tracepoint/") else {
                continue;
            };
            let Some(section) = rest.split('"').next() else {
                continue;
            };
            let Some((category, name)) = section.split_once('/') else {
                continue;
            };
            let Some(func) = lines.peek().and_then(|l| {
                l.trim()
                    .strip_prefix("int ")
                    .and_then(|s| s.split('(').next())
            }) else {
                continue;
            };
            declared.push((func.to_string(), category.to_string(), name.to_string()));
        }
        assert_eq!(
            declared.len(),
            PROGRAMS.len(),
            "the probe declares {} tracepoint programs, the attach table has {}",
            declared.len(),
            PROGRAMS.len()
        );
        for (func, category, name) in &declared {
            assert!(
                PROGRAMS
                    .iter()
                    .any(|(p, c, n)| p == func && c == category && n == name),
                "{func} ({category}:{name}) is in the probe but not in the attach table"
            );
        }
    }
}
