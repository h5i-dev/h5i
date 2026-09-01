//! Whose port is this?

// Only `process_tree` needs it, and that is a Darwin syscall walk, so on any
// other target this import is unused, which `-D warnings` makes fatal.
#[cfg(target_os = "macos")]
use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

// ─── the policy, which is pure and therefore tested ─────────────────────────

/// One listening TCP socket, as observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listener {
    pub pid: u32,
    pub addr: SocketAddr,
}

/// What h5i found when it asked who holds the port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ownership {
    /// The box holds it, unambiguously, at this address. Dial exactly this,
    /// not "loopback", *this*, because which socket a connection reaches
    /// depends on the address it was made to.
    Box { pid: u32, addr: SocketAddr },
    /// Nothing at all is listening on that port. The dev server has not started
    /// yet, which is a warning rather than a refusal: an agent about to start
    /// one is a perfectly good reason to share a port that is not up.
    Nobody,
    /// Something is listening and it is not this box. The refusal that matters:
    /// this is the case where a share would have published a stranger.
    Stranger { pid: u32, addr: SocketAddr },
    /// The box holds it, and so does something else, in a way that decides
    /// per connection which one answers. Two sockets on the same address via
    /// `SO_REUSEPORT`. Refused, because "usually the box" is not a promise
    /// worth making about what goes on the public internet.
    Contested { addr: SocketAddr, others: Vec<u32> },
}

/// Where h5i would dial, in the order it would prefer, and what beats what.
const DIAL_CANDIDATES: [IpAddr; 2] = [
    IpAddr::V4(Ipv4Addr::LOCALHOST),
    IpAddr::V6(Ipv6Addr::LOCALHOST),
];

/// How well a listening socket matches a dial address. Higher wins; `None` means
/// it cannot serve that address at all.
///
/// The dual-stack case is the one worth spelling out. A socket bound to `::`
/// with `IPV6_V6ONLY` off also accepts IPv4 connections, and libproc does not
/// report that flag, so a `::` listener is treated as a *possible* answerer for
/// an IPv4 dial at wildcard rank. Treating it as unable to answer would let a
/// stranger on `::` quietly take connections h5i had attributed to the box.
fn specificity(listen: &SocketAddr, dial: IpAddr) -> Option<u8> {
    let l = listen.ip();
    match (l, dial) {
        // Exactly the address being dialled.
        (a, b) if a == b => Some(2),
        // The wildcard of the same family.
        (IpAddr::V4(a), IpAddr::V4(_)) if a.is_unspecified() => Some(1),
        (IpAddr::V6(a), IpAddr::V6(_)) if a.is_unspecified() => Some(1),
        // A dual-stack `::` socket, for an IPv4 dial. See above.
        (IpAddr::V6(a), IpAddr::V4(_)) if a.is_unspecified() => Some(1),
        _ => None,
    }
}

/// Decide who answers, for one port, given every listener on it.
pub fn decide(listeners: &[Listener], port: u16, is_box: impl Fn(u32) -> bool) -> Ownership {
    let on_port: Vec<&Listener> = listeners.iter().filter(|l| l.addr.port() == port).collect();
    if on_port.is_empty() {
        return Ownership::Nobody;
    }

    // Dial in the family the box actually binds, and try that one first.
    let box_binds_v4 = on_port.iter().any(|l| is_box(l.pid) && l.addr.is_ipv4());
    let candidates: [IpAddr; 2] = if box_binds_v4 {
        DIAL_CANDIDATES
    } else {
        [DIAL_CANDIDATES[1], DIAL_CANDIDATES[0]]
    };

    let mut fallback: Option<Ownership> = None;
    for dial in candidates {
        // Everything that could answer a connection to this address, best match
        // first.
        let mut reachable: Vec<(u8, &Listener)> = on_port
            .iter()
            .filter_map(|l| specificity(&l.addr, dial).map(|s| (s, *l)))
            .collect();
        if reachable.is_empty() {
            continue;
        }
        let best = reachable.iter().map(|(s, _)| *s).max().unwrap_or(0);
        reachable.retain(|(s, _)| *s == best);

        // A tie at the winning rank is `SO_REUSEPORT`: the kernel spreads
        // connections across them and h5i cannot promise which one a visitor
        // gets.
        if reachable.len() > 1 {
            let ours = reachable.iter().any(|(_, l)| is_box(l.pid));
            let others: Vec<u32> = reachable
                .iter()
                .map(|(_, l)| l.pid)
                .filter(|p| !is_box(*p))
                .collect();
            // Unless every one of them is the box's. A tie is only a problem
            // because h5i cannot say *which* socket answers; when they all
            // belong to the box, that question has no wrong answer. A worker
            // pool sharing one port with `SO_REUSEPORT` is an ordinary way to
            // run a server, and refusing it would be refusing the box's own
            // dev server for being well written.
            if others.is_empty() {
                return Ownership::Box {
                    pid: reachable[0].1.pid,
                    addr: SocketAddr::new(dial, port),
                };
            }
            // Only interesting if the box is one of them; otherwise it is a
            // stranger's port and the stranger arm below says so more clearly.
            if ours && fallback.is_none() {
                fallback = Some(Ownership::Contested {
                    addr: SocketAddr::new(dial, port),
                    others,
                });
            }
            continue;
        }

        let winner = reachable[0].1;
        if is_box(winner.pid) {
            // Dial the address h5i just reasoned about, not the address the
            // socket is bound to: a box on `0.0.0.0:3000` is reached at
            // `127.0.0.1:3000`, and dialling `0.0.0.0` is not a thing.
            return Ownership::Box {
                pid: winner.pid,
                addr: SocketAddr::new(dial, port),
            };
        }
        if fallback.is_none() {
            fallback = Some(Ownership::Stranger {
                pid: winner.pid,
                addr: winner.addr,
            });
        }
    }

    // No candidate address the box wins. Report the most useful reason seen;
    // if the port is held only on addresses h5i would never dial (a LAN
    // address, say), that is still "not reachable as the box's port".
    fallback.unwrap_or_else(|| {
        let l = on_port[0];
        if is_box(l.pid) {
            // The box is listening, but somewhere h5i cannot dial it. A
            // specific LAN address and no loopback bind.
            Ownership::Nobody
        } else {
            Ownership::Stranger {
                pid: l.pid,
                addr: l.addr,
            }
        }
    })
}

/// Is the box's root still the process h5i pinned, or has its pid changed hands?
pub fn root_unchanged(pinned: u64, now: Option<u64>) -> bool {
    now == Some(pinned)
}

// ─── asking the kernel ──────────────────────────────────────────────────────

/// Every listening TCP socket this user can see, with the pid that holds it.
///
/// Errors are not propagated per process on purpose: a process that exits
/// between the pid list and the fd list, or one belonging to another user,
/// simply contributes nothing. That is the fail-closed direction, an
/// unattributed listener is never counted as the box's, and the alternative
/// would be a share that refuses because something unrelated on the machine
/// happened to exit at the wrong moment.
#[cfg(target_os = "macos")]
pub fn listening_sockets() -> Vec<Listener> {
    let mut out = Vec::new();
    for pid in all_pids() {
        for fd in fds_of(pid) {
            if fd.proc_fdtype != libc::PROX_FDTYPE_SOCKET as u32 {
                continue;
            }
            if let Some(addr) = listening_addr(pid, fd.proc_fd) {
                out.push(Listener { pid, addr });
            }
        }
    }
    out
}

/// The box's processes: the session h5i started, and everything under it.
///
/// The tree is walked from a snapshot of the whole process table rather than
/// per-pid, so a process that forks while this runs is either in the snapshot
/// or is a child of something in it, and the next call, a moment later, sees
/// it. The share re-checks on every dial, so that staleness costs nothing.
#[cfg(target_os = "macos")]
pub fn process_tree(root: u32) -> HashSet<u32> {
    let mut children: std::collections::HashMap<u32, Vec<u32>> = std::collections::HashMap::new();
    for pid in all_pids() {
        if let Some(ppid) = parent_of(pid) {
            children.entry(ppid).or_default().push(pid);
        }
    }
    let mut seen = HashSet::from([root]);
    let mut queue = std::collections::VecDeque::from([root]);
    while let Some(pid) = queue.pop_front() {
        for &child in children.get(&pid).map(Vec::as_slice).unwrap_or(&[]) {
            if seen.insert(child) {
                queue.push_back(child);
            }
        }
    }
    seen
}

/// A process's name, for a refusal that can be acted on.
///
/// "Port 3000 is held by something that is not the box" sends somebody hunting;
/// "held by `node` (pid 4820), which is not part of box `demo`" ends the hunt.
/// Best-effort by design. A process that exits first, or belongs to another
/// user, simply has no name here, and the refusal still stands on the pid.
#[cfg(target_os = "macos")]
pub fn process_name(pid: u32) -> Option<String> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    let got = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            size,
        )
    };
    if got != size {
        return None;
    }
    // `pbi_name` is the long form and is empty for some processes, where
    // `pbi_comm` (truncated to 16) is all there is.
    let read = |field: &[libc::c_char]| -> String {
        field
            .iter()
            .take_while(|&&c| c != 0)
            .map(|&c| c as u8 as char)
            .collect()
    };
    let name = read(&info.pbi_name);
    let name = if name.is_empty() {
        read(&info.pbi_comm)
    } else {
        name
    };
    display_name(&name)
}

/// A process name fit to print, which is the only thing this is ever used for.
///
/// The name comes from the kernel but it is *chosen by whoever started the
/// process*: it is the executable's file name, and a file name may contain any
/// byte but `/` and NUL. So this is untrusted input arriving through a trusted
/// channel, and it goes straight into the sentence h5i prints when it refuses to
/// share a port:
///
/// ```text
///   port 3000 on this machine is held by `<name>` (pid 421), which is not
///   part of this box, so h5i will not share it.
/// ```
///
/// That is the sentence the operator reads to work out what is wrong, and an
/// unsanitised name lets the process being complained about write escape
/// sequences into it: clearing the line, recolouring it, adding a clickable
/// OSC-8 hyperlink, or scrolling the refusal off the screen and leaving
/// something that reads like success. Measured rather than supposed: a binary
/// named with a literal `ESC [ 3 1 m` is reported by Darwin verbatim.
///
/// [`h5i_core::redact::sanitize_display`] is the repository's existing answer to
/// exactly this. Applied at the boundary rather than at each format site, since
/// rendering is all this value is for.
///
/// Compiled everywhere, though its only caller is Darwin's `process_name`:
/// nothing in it is platform-specific and the property it holds is a security
/// one, so the test below runs on every platform. The `allow` is what that
/// costs.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn display_name(raw: &str) -> Option<String> {
    let clean = h5i_core::redact::sanitize_display(raw);
    let clean = clean.trim().to_string();
    (!clean.is_empty()).then_some(clean)
}

/// Does this process still exist?
#[cfg(target_os = "macos")]
pub fn is_alive(pid: u32) -> bool {
    // SAFETY: signal 0 performs the permission and existence checks and sends
    // nothing.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

/// Is `pid` still under `root`, asked *now* and walked upwards?
#[cfg(target_os = "macos")]
pub fn is_descendant(pid: u32, root: u32) -> bool {
    let mut at = pid;
    for _ in 0..64 {
        if at == root {
            return true;
        }
        // pid 1 is launchd and 0 is the kernel: both are above any box.
        match parent_of(at) {
            Some(p) if p != 0 && p != at => at = p,
            _ => return false,
        }
    }
    false
}

/// Every pid on the machine, grown until the answer provably fits.
#[cfg(target_os = "macos")]
fn all_pids() -> Vec<u32> {
    /// Far past any attainable process count (`kern.maxproc` is in the
    /// thousands); the loop is bounded by construction, not by trust.
    const CEILING: usize = 1 << 20;

    let n = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if n <= 0 {
        return Vec::new();
    }
    let mut count = (n as usize).saturating_add(64).max(256);
    loop {
        let mut buf = vec![0i32; count];
        let bytes = (count * std::mem::size_of::<i32>()) as libc::c_int;
        let got = unsafe { libc::proc_listallpids(buf.as_mut_ptr() as *mut libc::c_void, bytes) };
        if got <= 0 {
            return Vec::new();
        }
        let n = got as usize;
        if n < count || count >= CEILING {
            buf.truncate(n.min(count));
            return buf
                .into_iter()
                .filter(|&p| p > 0)
                .map(|p| p as u32)
                .collect();
        }
        count = count.saturating_mul(2).min(CEILING);
    }
}

#[cfg(target_os = "macos")]
fn parent_of(pid: u32) -> Option<u32> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    let got = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            size,
        )
    };
    (got == size).then_some(info.pbi_ppid)
}

/// Every descriptor a process holds.
#[cfg(target_os = "macos")]
fn fds_of(pid: u32) -> Vec<libc::proc_fdinfo> {
    /// Far past any attainable descriptor limit (`proc_fdinfo` is 8 bytes, so
    /// this is 8 MiB), and present only so the loop is bounded by construction
    /// rather than by trust in a process's `RLIMIT_NOFILE`.
    const CEILING: usize = 1 << 20;

    let each = std::mem::size_of::<libc::proc_fdinfo>();
    let size = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDLISTFDS,
            0,
            std::ptr::null_mut(),
            0,
        )
    };
    if size <= 0 {
        return Vec::new();
    }
    let mut count = (size as usize / each).saturating_add(32).max(64);
    loop {
        let mut buf: Vec<libc::proc_fdinfo> = vec![unsafe { std::mem::zeroed() }; count];
        let got = unsafe {
            libc::proc_pidinfo(
                pid as libc::c_int,
                libc::PROC_PIDLISTFDS,
                0,
                buf.as_mut_ptr() as *mut libc::c_void,
                (count * each) as libc::c_int,
            )
        };
        if got <= 0 {
            return Vec::new();
        }
        let n = got as usize / each;
        if n < count || count >= CEILING {
            buf.truncate(n.min(count));
            return buf;
        }
        // Came back full, so the tail may have been cut. Ask again with room.
        count = count.saturating_mul(2).min(CEILING);
    }
}

/// `PROC_PIDFDSOCKETINFO`, read by offset.
///
/// The flavour returns `struct socket_fdinfo`, which libc does not define and
/// which is deliberately not hand-declared here: it nests `vinfo_stat`, two
/// `sockbuf_info`s and a seven-arm union, so a transcription wrong in its *tail*
/// is a struct the kernel overruns, and one wrong in its *middle* is a security
/// decision made on misread memory.
///
/// Instead the buffer is oversized and the four fields this needs are read at
/// fixed offsets, bounds-checked, from the bytes the kernel wrote. The offsets
/// are derived below and *proved* by
/// [`tests::the_offsets_find_a_socket_we_bound_ourselves`], which binds real
/// sockets on known addresses and asserts this reports them back.
///
/// ```text
/// struct socket_fdinfo {                   offset
///   struct proc_fileinfo pfi;                 0    24 bytes
///   struct socket_info   psi;                24
/// };
/// struct socket_info {                     (from 24)
///   struct vinfo_stat soi_stat;              +0    136 bytes
///   uint64_t soi_so, soi_pcb;              +136
///   int soi_type, soi_protocol, soi_family;+152
///   short soi_options … soi_timeo;         +164
///   u_short soi_error; uint32_t soi_oobmark;
///   struct sockbuf_info soi_rcv, soi_snd;  +184    24 bytes each
///   int soi_kind;                          +232
///   uint32_t rfu_1;                        +236
///   union { … } soi_proto;                 +240
/// };
/// struct in_sockinfo {                     (from 264 = 24 + 240)
///   int insi_fport, insi_lport;              +0
///   uint64_t insi_gencnt;                    +8
///   uint32_t insi_flags, insi_flow;         +16
///   uint8_t insi_vflag, insi_ip_ttl;        +24
///   uint32_t rfu_1;                         +28
///   union {…} insi_faddr;                   +32    16 bytes
///   union {…} insi_laddr;                   +48    16 bytes
///   struct { u_char in4_tos; } insi_v4;     +64
///   struct { uint8_t in6_hlim; int in6_cksum;
///            u_short in6_ifindex;
///            short in6_hops; } insi_v6;     +68    12 bytes
/// };                                        = 80, 8-aligned
/// struct tcp_sockinfo { struct in_sockinfo tcpsi_ini; int tcpsi_state; … };
/// ```
///
/// The last two rows are easy to omit, and were. `insi_laddr` is the last field
/// anything here reads, so a table stopping there looks complete while putting
/// `in_sockinfo` at 64 bytes and `tcpsi_state` 16 short of where it is.
#[cfg(target_os = "macos")]
fn listening_addr(pid: u32, fd: i32) -> Option<SocketAddr> {
    /// `PROC_PIDFDSOCKETINFO`. Not in libc, and its value is part of the
    /// stable `proc_info` interface.
    const PROC_PIDFDSOCKETINFO: libc::c_int = 3;
    /// `soi_kind` for a TCP socket, and the TCP state that means "listening"
    /// (`TCPS_LISTEN` from `netinet/tcp_fsm.h`).
    const SOCKINFO_TCP: i32 = 2;
    const TCPS_LISTEN: i32 = 1;
    /// `insi_vflag`.
    const INI_IPV4: u8 = 0x1;
    const INI_IPV6: u8 = 0x2;

    const SOI_KIND: usize = 24 + 232;
    const SOI_FAMILY: usize = 24 + 160;
    const PROTO: usize = 24 + 240;
    const INSI_LPORT: usize = PROTO + 4;
    const INSI_VFLAG: usize = PROTO + 24;
    const INSI_LADDR: usize = PROTO + 48;
    const TCPSI_STATE: usize = PROTO + 80;

    // Comfortably larger than the struct (~700 bytes on every Darwin this
    // runs on). The kernel refuses the call outright if the buffer is too
    // small, so slack here is the difference between working and returning
    // nothing. Never between working and being overrun.
    let mut buf = [0u8; 2048];
    let got = unsafe {
        libc::proc_pidfdinfo(
            pid as libc::c_int,
            fd,
            PROC_PIDFDSOCKETINFO,
            buf.as_mut_ptr() as *mut libc::c_void,
            buf.len() as libc::c_int,
        )
    };
    // A short answer is not a partially usable one.
    if got < TCPSI_STATE as libc::c_int + 4 {
        return None;
    }

    // Every offset read here is a compile-time constant well inside `buf`, so
    // this cannot be out of bounds; the length check above is about what the
    // *kernel* wrote, not about what can be indexed.
    let i32_at = |off: usize| -> i32 {
        i32::from_ne_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
    };
    // Only TCP, and only while listening. A connected socket on the same port
    // is not something a visitor can be handed.
    if i32_at(SOI_KIND) != SOCKINFO_TCP || i32_at(TCPSI_STATE) != TCPS_LISTEN {
        return None;
    }
    // Network byte order, read as bytes rather than swapped from a native
    // `int`, so the endianness is stated where it happens.
    let port = u16::from_be_bytes([buf[INSI_LPORT], buf[INSI_LPORT + 1]]);
    if port == 0 {
        return None;
    }

    // `insi_vflag` decides how to read the address, which union arm the kernel
    // filled in, and `soi_family` does not, because the family says how the
    // socket was created rather than which address space it ended up in. The
    // five shapes, measured on Darwin rather than reasoned about:
    //
    // ```text
    //   bind                    soi_family   insi_vflag   insi_laddr
    //   127.0.0.1               AF_INET (2)  0x01         ..00 7f 00 00 01
    //   0.0.0.0                 AF_INET      0x01         all zero
    //   [::1]                   AF_INET6(30) 0x02         ..00 00 00 00 01
    //   [::]  (dual-stack)      AF_INET6     0x03         all zero
    //   [::ffff:127.0.0.1]      AF_INET6     0x01         ..00 7f 00 00 01
    // ```
    //
    // The last row is why this is not a matter of taste. A v4-mapped bind is an
    // AF_INET6 socket holding an *IPv4* address, in `in4in6_addr` form and
    // without the `ffff`, so reading the v6 arm for it reports `::7f00:1`, an
    // address that exists nowhere. `decide` then ranks it unreachable and cannot
    // see it at all, and an invisible listener is the dangerous kind.
    //
    // Keying on `insi_vflag` gets all five right, and the order matters: the
    // dual-stack row has *both* bits, and it is the v6 arm that describes it.
    let vflag = buf[INSI_VFLAG];
    let v6 = || -> Option<IpAddr> {
        let b: [u8; 16] = buf[INSI_LADDR..INSI_LADDR + 16].try_into().ok()?;
        Some(IpAddr::V6(Ipv6Addr::from(b)))
    };
    let v4 = || -> Option<IpAddr> {
        // `in4in6_addr`: three words of padding, then the v4 address.
        let b: [u8; 4] = buf[INSI_LADDR + 12..INSI_LADDR + 16].try_into().ok()?;
        Some(IpAddr::V4(Ipv4Addr::from(b)))
    };
    let ip = if vflag & INI_IPV6 != 0 {
        v6()?
    } else if vflag & INI_IPV4 != 0 {
        v4()?
    } else {
        // No flag set at all. Fall back to the socket's domain rather than
        // guessing an arm, and give up if that says nothing either.
        match i32_at(SOI_FAMILY) {
            libc::AF_INET6 => v6()?,
            libc::AF_INET => v4()?,
            _ => return None,
        }
    };
    Some(SocketAddr::new(ip, port))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn l(pid: u32, addr: &str) -> Listener {
        Listener {
            pid,
            addr: addr.parse().expect("test address"),
        }
    }

    /// A root whose pid has changed hands is not the box.
    ///
    /// `is_descendant(pid, root)` is true the instant `pid == root`, and
    /// `process_tree` seeds its set with `root`, so whoever holds that pid is the
    /// box wholesale. h5i verifies it when it resolves it and again on its
    /// three-second poll; between those, the dialer held a bare number and every
    /// dial trusted it.
    #[test]
    fn a_root_pid_that_changed_hands_is_not_the_box() {
        assert!(root_unchanged(1234, Some(1234)), "the same process");
        assert!(
            !root_unchanged(1234, Some(9999)),
            "a later tenant of the pid was taken for the box"
        );
        // Gone reads as changed, not as unchanged: "cannot tell" is not the
        // same answer as "still ours", and this is the direction that decides
        // whether a stranger gets published.
        assert!(
            !root_unchanged(1234, None),
            "a root that could not be read was taken for the box"
        );
        // Zero is a start time like any other, not a sentinel.
        assert!(root_unchanged(0, Some(0)));
    }

    /// The box, as the predicate `decide` now asks rather than the set it used
    /// to consult. Tests answer from a fixed list; the dialer answers from a
    /// snapshot plus a live ancestry walk.
    fn boxed(pids: &[u32]) -> impl Fn(u32) -> bool + '_ {
        move |pid| pids.contains(&pid)
    }

    #[test]
    fn nothing_listening_is_its_own_answer() {
        assert_eq!(decide(&[], 3000, boxed(&[10])), Ownership::Nobody);
        // Something on another port is not something on this one.
        assert_eq!(
            decide(&[l(10, "127.0.0.1:5173")], 3000, boxed(&[10])),
            Ownership::Nobody
        );
    }

    #[test]
    fn the_box_on_loopback_is_dialled_there() {
        assert_eq!(
            decide(&[l(10, "127.0.0.1:3000")], 3000, boxed(&[10])),
            Ownership::Box {
                pid: 10,
                addr: "127.0.0.1:3000".parse().unwrap()
            }
        );
    }

    #[test]
    fn a_wildcard_box_is_dialled_on_loopback_not_on_the_wildcard() {
        // `0.0.0.0` is what the socket is bound to; it is not an address to
        // connect to.
        assert_eq!(
            decide(&[l(10, "0.0.0.0:3000")], 3000, boxed(&[10])),
            Ownership::Box {
                pid: 10,
                addr: "127.0.0.1:3000".parse().unwrap()
            }
        );
    }

    #[test]
    fn a_stranger_holding_the_port_is_refused_not_shared() {
        // The whole point of the module.
        assert_eq!(
            decide(&[l(99, "127.0.0.1:3000")], 3000, boxed(&[10])),
            Ownership::Stranger {
                pid: 99,
                addr: "127.0.0.1:3000".parse().unwrap()
            }
        );
    }

    #[test]
    fn a_specific_stranger_beats_a_wildcard_box_and_that_is_refused() {
        // The situation this module was written for, and the one a plain
        // `connect("127.0.0.1", 3000)` gets wrong: the box holds the IPv6
        // wildcard, a stranger holds IPv4 loopback exactly. Dialling
        // 127.0.0.1 reaches the stranger, so IPv4 is not offered, and the
        // box is found on `::1`, where it wins.
        let got = decide(
            &[l(10, "[::]:3000"), l(99, "127.0.0.1:3000")],
            3000,
            boxed(&[10]),
        );
        assert_eq!(
            got,
            Ownership::Box {
                pid: 10,
                addr: "[::1]:3000".parse().unwrap()
            },
            "the box must be reached where it actually wins"
        );
    }

    #[test]
    fn a_stranger_on_both_loopbacks_leaves_the_box_nowhere_to_be_dialled() {
        let got = decide(
            &[
                l(10, "0.0.0.0:3000"),
                l(99, "127.0.0.1:3000"),
                l(98, "[::1]:3000"),
            ],
            3000,
            boxed(&[10]),
        );
        assert!(
            matches!(got, Ownership::Stranger { .. }),
            "no candidate address the box wins, so this must refuse: {got:?}"
        );
    }

    #[test]
    fn two_sockets_on_one_address_are_contested_not_resolved_in_our_favour() {
        // `SO_REUSEPORT`: the kernel decides per connection. "Usually the box"
        // is not a promise to make about the public internet.
        let got = decide(
            &[l(10, "127.0.0.1:3000"), l(99, "127.0.0.1:3000")],
            3000,
            boxed(&[10]),
        );
        match got {
            Ownership::Contested { addr, others } => {
                assert_eq!(addr, "127.0.0.1:3000".parse().unwrap());
                assert_eq!(others, vec![99]);
            }
            other => panic!("a shared address must be refused, got {other:?}"),
        }
    }

    #[test]
    fn a_box_on_the_v6_wildcard_alone_is_dialled_on_v6() {
        // `[::]` covers IPv4 too *if* `IPV6_V6ONLY` is off, and nothing h5i can
        // read says which it is. Dialling `127.0.0.1` is therefore a guess that
        // is wrong half the time and fails as `ECONNREFUSED`. Reported to the
        // owner of a perfectly good server as "nothing is listening". `[::1]`
        // is right either way.
        assert_eq!(
            decide(&[l(10, "[::]:3000")], 3000, boxed(&[10])),
            Ownership::Box {
                pid: 10,
                addr: "[::1]:3000".parse().unwrap()
            }
        );
    }

    #[test]
    fn a_worker_pool_sharing_one_address_is_the_box_not_a_contest() {
        // Two box processes on one address via `SO_REUSEPORT`. How a good
        // many servers run. The kernel picks one per connection and both are
        // the box, so there is nothing to refuse; treating this as contested
        // would refuse a dev server for being multi-process.
        let got = decide(
            &[l(10, "127.0.0.1:3000"), l(11, "127.0.0.1:3000")],
            3000,
            boxed(&[10, 11]),
        );
        match got {
            Ownership::Box { addr, pid } => {
                assert_eq!(addr, "127.0.0.1:3000".parse().unwrap());
                assert!(pid == 10 || pid == 11, "either worker is the box");
            }
            other => panic!("all-box listeners must not be a contest: {other:?}"),
        }
    }

    #[test]
    fn a_box_that_holds_both_families_is_still_just_the_box() {
        let got = decide(
            &[l(10, "127.0.0.1:3000"), l(10, "[::1]:3000")],
            3000,
            boxed(&[10]),
        );
        assert_eq!(
            got,
            Ownership::Box {
                pid: 10,
                addr: "127.0.0.1:3000".parse().unwrap()
            }
        );
    }

    #[test]
    fn a_box_child_holds_the_port_not_the_session_leader() {
        // The listener is the dev server, which is a descendant of the session
        // h5i started. Never the session process itself.
        assert_eq!(
            decide(&[l(14508, "127.0.0.1:3000")], 3000, boxed(&[14506, 14508])),
            Ownership::Box {
                pid: 14508,
                addr: "127.0.0.1:3000".parse().unwrap()
            }
        );
    }

    /// The ABI test: bind real sockets, then ask the reader to find them.
    ///
    /// This is what stands in for a hand-declared `socket_fdinfo`. Every offset
    /// in [`listening_addr`] has to be right for this to pass. A wrong one
    /// fails the TCP/listening check and the socket is simply not found.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_offsets_find_a_socket_we_bound_ourselves() {
        use std::net::{TcpListener, TcpStream};

        let me = std::process::id();
        let v4 = TcpListener::bind("127.0.0.1:0").expect("bind v4 loopback");
        let v6 = TcpListener::bind("[::1]:0").expect("bind v6 loopback");
        let wild = TcpListener::bind("0.0.0.0:0").expect("bind v4 wildcard");
        let (p4, p6, pw) = (
            v4.local_addr().unwrap().port(),
            v6.local_addr().unwrap().port(),
            wild.local_addr().unwrap().port(),
        );

        let found = listening_sockets();
        let mine: Vec<&Listener> = found.iter().filter(|l| l.pid == me).collect();
        assert!(
            mine.iter()
                .any(|l| l.addr == format!("127.0.0.1:{p4}").parse().unwrap()),
            "the v4 loopback listener on {p4} was not found among {mine:?}"
        );
        assert!(
            mine.iter()
                .any(|l| l.addr == format!("[::1]:{p6}").parse().unwrap()),
            "the v6 loopback listener on {p6} was not found among {mine:?}"
        );
        assert!(
            mine.iter()
                .any(|l| l.addr == format!("0.0.0.0:{pw}").parse().unwrap()),
            "the wildcard listener on {pw} was not found among {mine:?}"
        );

        // A *connected* socket is not a listening one, and must not be
        // reported: handing a visitor an established connection is not a thing
        // that can work.
        let peer = TcpStream::connect(("127.0.0.1", p4)).expect("connect");
        let (_accepted, _) = v4.accept().expect("accept");
        let after = listening_sockets();
        let local = peer.local_addr().unwrap();
        assert!(
            !after.iter().any(|l| l.pid == me && l.addr == local),
            "an established connection was reported as a listener"
        );
    }

    /// What Darwin calls a dual-stack `::` socket, which decides whether
    /// [`specificity`]'s dual-stack arm is reasoning about a shape that exists.
    ///
    /// Rust's `TcpListener::bind("[::]:0")` leaves `IPV6_V6ONLY` off, so this
    /// one socket accepts both families. The question is what libproc reports
    /// it as, and the answer drives the rule: reported as `::`, the dual-stack
    /// arm is what stops a stranger on `::` from quietly taking an IPv4
    /// connection h5i attributed to the box.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_dual_stack_wildcard_is_reported_as_the_v6_wildcard() {
        use std::net::TcpListener;

        let dual = TcpListener::bind("[::]:0").expect("bind the v6 wildcard");
        let port = dual.local_addr().unwrap().port();
        let me = std::process::id();

        let found = listening_sockets();
        let ours: Vec<&Listener> = found
            .iter()
            .filter(|l| l.pid == me && l.addr.port() == port)
            .collect();
        assert_eq!(
            ours.len(),
            1,
            "one bind is one socket, however many families it serves: {ours:?}"
        );
        assert_eq!(
            ours[0].addr.ip(),
            IpAddr::V6(Ipv6Addr::UNSPECIFIED),
            "a dual-stack socket must be reported as `::`, which is what the \
             dual-stack arm of `specificity` is written against"
        );

        // And the rule built on it: this socket really does answer an IPv4
        // loopback connection, which is why `::` ranks as a wildcard for an
        // IPv4 dial rather than as unable to serve one.
        let peer = std::net::TcpStream::connect(("127.0.0.1", port));
        assert!(
            peer.is_ok(),
            "a dual-stack `::` listener answers 127.0.0.1, so h5i must treat it \
             as a contender for an IPv4 dial: {peer:?}"
        );
    }

    /// Every bind shape Darwin can report, and the address h5i must read back.
    ///
    /// The table in [`listening_addr`] as a test, because getting it wrong is
    /// not a cosmetic error: a listener h5i reads as the wrong address is a
    /// listener [`decide`] cannot rank, and one it cannot rank is one it cannot
    /// refuse. The v4-mapped row is the one that was wrong. Reported as
    /// `::7f00:1`, an address that exists nowhere, while the socket really
    /// answers connections to `127.0.0.1`.
    #[cfg(target_os = "macos")]
    #[test]
    fn every_bind_shape_reads_back_as_the_address_it_serves() {
        use std::net::TcpListener;

        let me = std::process::id();
        // `[::ffff:…]` is last: it is the one that binds into the v4 space, so
        // it would collide with the first row's port if they shared one.
        let cases: [(&str, &dyn Fn(std::net::SocketAddr) -> String); 5] = [
            ("127.0.0.1:0", &|a| format!("127.0.0.1:{}", a.port())),
            ("0.0.0.0:0", &|a| format!("0.0.0.0:{}", a.port())),
            ("[::1]:0", &|a| format!("[::1]:{}", a.port())),
            ("[::]:0", &|a| format!("[::]:{}", a.port())),
            // An AF_INET6 socket holding an IPv4 address: what it serves is
            // `127.0.0.1`, and that is what h5i has to see.
            ("[::ffff:127.0.0.1]:0", &|a| {
                format!("127.0.0.1:{}", a.port())
            }),
        ];

        for (spec, want) in cases {
            let Ok(l) = TcpListener::bind(spec) else {
                panic!("could not bind {spec} to check how it reads back");
            };
            let bound = l.local_addr().expect("local addr");
            let expected: SocketAddr = want(bound).parse().expect("expected address");
            let found: Vec<SocketAddr> = listening_sockets()
                .into_iter()
                .filter(|x| x.pid == me && x.addr.port() == bound.port())
                .map(|x| x.addr)
                .collect();
            assert!(
                found.contains(&expected),
                "a socket bound {spec} (kernel says {bound}) must read back as {expected}, \
                 and read back as {found:?}"
            );
        }
    }

    /// A process cannot write escape sequences into h5i's refusal.
    #[test]
    fn a_process_name_cannot_carry_escapes_into_the_refusal() {
        // The exact bytes measured from the kernel for the compiled probe.
        let hostile = "ev\u{1b}[31mIL";
        let shown = display_name(hostile).expect("a name that is not only controls");
        assert!(
            !shown.chars().any(|c| c.is_control()),
            "an escape survived into the refusal: {shown:?}"
        );
        assert_eq!(shown, "ev[31mIL", "only the control byte should go");

        // The sharper spoof needs no escape at all: a right-to-left override
        // reorders the text around it, so a name can be made to read as another.
        let bidi = display_name("evil\u{202E}drowssap").expect("a name");
        assert!(
            !bidi.contains('\u{202E}'),
            "a bidi override survived: {bidi:?}"
        );

        // A newline would let a name forge a second line of output.
        let multiline = display_name("evil\nOK  shared port 3000").expect("a name");
        assert!(
            !multiline.contains('\n'),
            "a name forged a line: {multiline:?}"
        );

        // And a name that was only controls has nothing left to print, which
        // must read as "no name" rather than as an empty pair of backticks.
        assert_eq!(display_name("\u{1b}\u{7f}"), None);
    }

    /// A listener behind a great many descriptors is still found.
    ///
    /// The hole this guards is that the kernel does not say "there was more": a
    /// buffer that comes back full is indistinguishable from a complete answer,
    /// and the fd list is ordered, so what a full buffer drops is the highest
    /// descriptors, where a process that wants to hide a socket puts it. The race
    /// itself cannot be staged deterministically; the scale can.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_listener_past_the_first_guess_at_a_buffer_is_still_found() {
        use std::net::TcpListener;

        // Well past the old fixed slack of 32, and past the usual 256-fd soft
        // limit, so the first sizing guess cannot happen to be enough.
        let mut held = Vec::new();
        for _ in 0..600 {
            match std::fs::File::open("/dev/null") {
                Ok(f) => held.push(f),
                // A low `RLIMIT_NOFILE` is not a failure of the thing under
                // test; whatever we did open still exercises it.
                Err(_) => break,
            }
        }
        assert!(
            held.len() > 100,
            "could not open enough descriptors to make this meaningful ({})",
            held.len()
        );

        // Bound *after* the flood, so it takes a high descriptor number. The
        // end of the list, which is what truncation removes.
        let late = TcpListener::bind("127.0.0.1:0").expect("bind behind the flood");
        let port = late.local_addr().expect("addr").port();
        let me = std::process::id();

        assert!(
            listening_sockets()
                .iter()
                .any(|l| l.pid == me && l.addr.port() == port),
            "a listener opened behind {} descriptors was not found — the scan stopped short",
            held.len()
        );
        drop(held);
    }

    /// The upward check, which is what the dialer trusts at the last moment.
    #[cfg(target_os = "macos")]
    #[test]
    fn ancestry_answers_for_a_grandchild_and_refuses_a_stranger() {
        let mut child = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("sleep 30 & wait")
            .spawn()
            .expect("spawn a child");
        let me = std::process::id();
        let mut grandchild = None;
        for _ in 0..100 {
            if let Some(g) = all_pids()
                .into_iter()
                .find(|p| parent_of(*p) == Some(child.id()))
            {
                grandchild = Some(g);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let grandchild = grandchild.expect("the shell never started its background `sleep`");

        assert!(is_descendant(me, me), "a process is its own root");
        assert!(is_descendant(child.id(), me), "a child is under its parent");
        assert!(
            is_descendant(grandchild, me),
            "a grandchild is under its grandparent — where a dev server lives"
        );
        // The direction that matters: launchd is not under us, and neither is
        // anything whose ancestry does not lead back to the root. A pid that
        // changed hands looks exactly like this.
        assert!(!is_descendant(1, me), "launchd is not part of this box");
        assert!(
            !is_descendant(me, child.id()),
            "ancestry is not symmetric, or every process would be in every box"
        );

        let _ = child.kill();
        let _ = child.wait();
        unsafe { libc::kill(grandchild as libc::pid_t, libc::SIGKILL) };
    }

    /// The tree walk, against processes we actually fork. Two deep.
    ///
    /// Depth is the point, not decoration. A box is `h5i box shell` running a
    /// shell running a dev server, so the listener h5i has to attribute is
    /// typically a *grandchild* of the session it is rooted at; a walk that only
    /// found direct children would refuse every real share while passing a
    /// one-level test. `sh -c 'sleep … & wait'` is written that way on purpose:
    /// `sh -c 'sleep …'` alone execs into `sleep` and leaves one level behind.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_process_tree_reaches_a_grandchild_and_stops_at_the_root() {
        let mut child = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("sleep 30 & wait")
            .spawn()
            .expect("spawn a child");
        let me = std::process::id();
        // The grandchild has to exist before the tree is walked.
        let mut grandchild = None;
        for _ in 0..100 {
            let found: Vec<u32> = all_pids()
                .into_iter()
                .filter(|p| parent_of(*p) == Some(child.id()))
                .collect();
            if let Some(g) = found.first() {
                grandchild = Some(*g);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let grandchild = grandchild.expect("the shell never started its background `sleep`");

        let tree = process_tree(me);
        assert!(tree.contains(&me), "the root itself is in its own tree");
        assert!(
            tree.contains(&child.id()),
            "a child of this process must be in its tree"
        );
        assert!(
            tree.contains(&grandchild),
            "a grandchild must be in the tree — this is where a box's dev server lives"
        );
        assert!(
            !tree.contains(&1),
            "launchd is not a descendant of this test"
        );
        let _ = child.kill();
        let _ = child.wait();
        unsafe { libc::kill(grandchild as libc::pid_t, libc::SIGKILL) };
    }
}

#[cfg(test)]
mod ownership_fuzz {
    use super::*;
    use crate::fuzz::{rounds, Rng};

    /// The addresses a listener on a shared machine can hold, including every
    /// spelling of "the same place" that `specificity` has to tell apart.
    const ADDRS: &[&str] = &[
        "127.0.0.1",
        "0.0.0.0",
        "[::1]",
        "[::]",
        "127.0.0.2",
        "192.168.1.5",
        "[fe80::1]",
        "[2001:db8::1]",
    ];

    /// Which listener the kernel hands a connection to `dial`.
    fn who_answers<'a>(on_port: &[&'a Listener], dial: IpAddr) -> Vec<&'a Listener> {
        let rank_of = |listen: IpAddr| -> Option<u8> {
            match (listen, dial) {
                // The exact address, whichever family.
                (a, b) if a == b => Some(2),
                // `0.0.0.0` answers any IPv4 connection.
                (IpAddr::V4(a), IpAddr::V4(_)) if a.is_unspecified() => Some(1),
                // `::` answers any IPv6 connection, and any IPv4 one too when
                // `IPV6_V6ONLY` is off, which libproc does not report, so it
                // has to be treated as possible.
                (IpAddr::V6(a), _) if a.is_unspecified() => Some(1),
                _ => None,
            }
        };
        let mut best: Vec<&Listener> = Vec::new();
        let mut rank = 0u8;
        for l in on_port {
            let Some(s) = rank_of(l.addr.ip()) else {
                continue;
            };
            if s > rank {
                rank = s;
                best.clear();
            }
            if s == rank {
                best.push(l);
            }
        }
        best
    }

    /// Whatever h5i decides to dial, a stranger cannot be the one that answers.
    ///
    /// This is the whole macOS safety argument, Linux gets it from a namespace
    /// and here it is established by observation, and it had only the cases
    /// somebody wrote down. The generator makes the shapes nobody would:
    /// wildcards against exact binds, both families at once, `SO_REUSEPORT`
    /// ties, a box that holds three sockets and a stranger that holds one.
    #[test]
    fn the_box_is_never_named_for_an_address_a_stranger_could_answer() {
        let mut rng = Rng::new(0x0BADCAFE);
        let mut named = 0usize;
        let mut refused = 0usize;
        for i in 0..rounds() {
            let seed = rng.next();
            let mut one = Rng::new(seed);

            // A handful of listeners over two pid families, on two ports so
            // that "the wrong port" is in the corpus too.
            let mut listeners: Vec<Listener> = Vec::new();
            for _ in 0..one.below(6) {
                let pid = if one.chance(2) {
                    10 + one.below(3) as u32
                } else {
                    90 + one.below(3) as u32
                };
                let port = if one.chance(4) { 5173 } else { 3000 };
                let addr = format!("{}:{port}", one.pick(ADDRS));
                listeners.push(Listener {
                    pid,
                    addr: addr.parse().expect("address"),
                });
            }
            // The box is the 10s. Everything else is a stranger.
            let is_box = |pid: u32| (10..13).contains(&pid);
            let ctx = || format!("round {i}, seed {seed:#x}, listeners {listeners:?}");

            match decide(&listeners, 3000, is_box) {
                Ownership::Box { pid, addr } => {
                    named += 1;
                    assert!(is_box(pid), "a stranger was named as the box: {}", ctx());
                    assert_eq!(addr.port(), 3000, "{}", ctx());
                    // The property. Everything that could answer a connection
                    // to the address h5i is about to dial has to be the box's,
                    // not merely the best of them, because a tie is decided by
                    // the kernel and h5i cannot promise which way.
                    let on_port: Vec<&Listener> =
                        listeners.iter().filter(|l| l.addr.port() == 3000).collect();
                    let answering = who_answers(&on_port, addr.ip());
                    assert!(
                        !answering.is_empty(),
                        "h5i would dial an address nothing answers: {} -> {addr}",
                        ctx()
                    );
                    for l in &answering {
                        assert!(
                            is_box(l.pid),
                            "a stranger could answer the address h5i named ({addr}): {} -> {l:?}",
                            ctx()
                        );
                    }
                }
                Ownership::Nobody => {
                    // Only when no candidate address the box wins outright.
                    // Never while the box is the sole answerer of one.
                    for dial in DIAL_CANDIDATES {
                        let on_port: Vec<&Listener> =
                            listeners.iter().filter(|l| l.addr.port() == 3000).collect();
                        let answering = who_answers(&on_port, dial);
                        assert!(
                            answering.is_empty() || answering.iter().any(|l| !is_box(l.pid)),
                            "the box owned {dial} outright and h5i said nothing was there: {}",
                            ctx()
                        );
                    }
                }
                Ownership::Stranger { pid, .. } => {
                    refused += 1;
                    assert!(
                        !is_box(pid),
                        "the box was reported as a stranger: {}",
                        ctx()
                    );
                }
                Ownership::Contested { others, .. } => {
                    refused += 1;
                    assert!(!others.is_empty(), "a contest with nobody in it: {}", ctx());
                    for pid in &others {
                        assert!(!is_box(*pid), "the box contested itself: {}", ctx());
                    }
                }
            }
        }

        let n = rounds();
        if n < 1_000 {
            return;
        }
        assert!(
            named * 20 > n,
            "the generator almost never produced a box h5i would dial: {named} of {n}"
        );
        assert!(
            refused * 50 > n,
            "the generator almost never produced a port h5i must refuse, so the half of \
             this that keeps a stranger off the internet was barely exercised: {refused} of {n}"
        );
    }
}
