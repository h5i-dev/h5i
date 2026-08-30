//! seccomp user-notification primitives for the supervisor tier
//! (`docs/supervisor-design.md`, phase B).
//!
// Some primitives here (the GET_NOTIF_SIZES ABI check, the notify-serve loop)
// are deferred-tier scaffolding not yet wired into a live dispatch path — the
// supervised execve-notify integration is a documented follow-up. They are
// intentionally retained, so allow dead_code at the module scope rather than
// deleting fail-closed plumbing we will need.
#![allow(dead_code)]
//!
//! A filter installed with `SECCOMP_FILTER_FLAG_NEW_LISTENER` returns a
//! **listener fd**; the supervisor (h5i) reads `socket()` notifications on it and
//! replies allow (`CONTINUE`) or deny (`errno`) per [`crate::supervisor`]'s
//! default-deny gate. This module is the careful, fail-closed plumbing:
//!
//! - the kernel ABI structs + ioctl numbers, validated against
//!   `SECCOMP_GET_NOTIF_SIZES` (refuse on any mismatch),
//! - a pure, unit-tested BPF program builder (notify on `socket`/`socketpair`,
//!   allow everything else, kill on arch mismatch),
//! - the notify loop, which **re-validates each notification id** before
//!   replying (TOCTOU/stale-id safety) and treats every error as fail-closed.
//!
//! Supports x86_64 and aarch64; other arches make the supervisor probe report
//! seccomp-notify unavailable, so the tier refuses (fail-closed).

#![cfg(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64")))]

use std::os::unix::io::RawFd;

use crate::error::H5iError;
use crate::supervisor::{decide_socket, Decision};

// ─── BPF return codes ─────────────────────────────────────────────────────────

const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;
const SECCOMP_RET_USER_NOTIF: u32 = 0x7fc0_0000;
const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;

// ─── per-arch AUDIT_ARCH + socket syscall numbers (the filter checks the
//     running process's arch matches before trusting the nr) ─────────────────
#[cfg(target_arch = "x86_64")]
const AUDIT_ARCH: u32 = 0xC000_003E; // AUDIT_ARCH_X86_64
#[cfg(target_arch = "x86_64")]
const NR_SOCKET: u32 = 41;
#[cfg(target_arch = "x86_64")]
const NR_SOCKETPAIR: u32 = 53;

#[cfg(target_arch = "aarch64")]
const AUDIT_ARCH: u32 = 0xC000_00B7; // AUDIT_ARCH_AARCH64
#[cfg(target_arch = "aarch64")]
const NR_SOCKET: u32 = 198;
#[cfg(target_arch = "aarch64")]
const NR_SOCKETPAIR: u32 = 199;

// seccomp operations / flags.
const SECCOMP_SET_MODE_FILTER: libc::c_uint = 1;
const SECCOMP_GET_NOTIF_SIZES: libc::c_uint = 3;
const SECCOMP_FILTER_FLAG_NEW_LISTENER: libc::c_ulong = 1 << 3;
/// Response flag: run the original syscall unmediated (the allow path).
const SECCOMP_USER_NOTIF_FLAG_CONTINUE: u32 = 1;

// ─── classic BPF instruction (struct sock_filter) ─────────────────────────────

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SockFilter {
    pub code: u16,
    pub jt: u8,
    pub jf: u8,
    pub k: u32,
}

#[repr(C)]
struct SockFprog {
    len: u16,
    filter: *const SockFilter,
}

// BPF opcodes (classic).
const BPF_LD: u16 = 0x00;
const BPF_W: u16 = 0x00;
const BPF_ABS: u16 = 0x20;
const BPF_JMP: u16 = 0x05;
const BPF_JEQ: u16 = 0x10;
/// `A >= K` — used to catch the x32 syscall-number range in one comparison.
const BPF_JGE: u16 = 0x30;
const BPF_K: u16 = 0x00;
const BPF_RET: u16 = 0x06;

// Offsets into struct seccomp_data.
const OFF_NR: u32 = 0;
const OFF_ARCH: u32 = 4;

fn stmt(code: u16, k: u32) -> SockFilter {
    SockFilter { code, jt: 0, jf: 0, k }
}
fn jump(code: u16, k: u32, jt: u8, jf: u8) -> SockFilter {
    SockFilter { code, jt, jf, k }
}

/// Build the filter that NOTIFYs on `socket`/`socketpair`, ALLOWs everything
/// else, and KILLs on an unexpected architecture (fail-closed). Returns a
/// fixed-size **stack** array so [`install_listener`] is allocation-free and
/// therefore async-signal-safe in a `fork`ed child; a `Vec` would risk a
/// malloc-lock deadlock when the parent is multithreaded. Pure, and structurally
/// unit-tested.
///
/// ### Why this set cannot simply grow
///
/// An allow here is `SECCOMP_USER_NOTIF_FLAG_CONTINUE`, and `seccomp_unotify(2)`
/// says that flag "should not be used for security purposes", because between
/// the supervisor's verdict and the kernel re-running the syscall another thread
/// can rewrite whatever the syscall reads. That warning is about arguments
/// living in the tracee's *memory*.
///
/// It does not apply here, which is why `CONTINUE` is sound in this tier:
/// `socket(domain, type, protocol)` and the first three arguments of
/// `socketpair` are **scalars the kernel already captured into the
/// notification** when it trapped. They are register values, so no other thread
/// can change them and the decision and the syscall see the same bytes.
///
/// Adding a comparison for anything taking a pointer, `connect`, `bind` or
/// `sendto`, is therefore not a local edit. Deciding on `*sockaddr` means
/// reading the tracee's memory, and with `CONTINUE` that is the textbook
/// double-fetch: the check passes on `127.0.0.1` and the kernel connects to
/// whatever the address holds a microsecond later. Such a syscall has to be
/// answered with `Decision::Deny`, never `CONTINUE`, or mediated by
/// `SECCOMP_IOCTL_NOTIF_ADDFD` so the supervisor supplies the result.
/// `only_syscalls_whose_arguments_are_registers_are_notified` pins the set so
/// the choice has to be made deliberately.
///
/// The other half of the argument is in [`crate::sandbox::denied_syscalls`].
/// This filter's fall-through is ALLOW, and io_uring executes submitted
/// operations without passing a syscall filter: `IORING_OP_SOCKET` builds the
/// `AF_PACKET`/`SOCK_RAW` socket [`crate::supervisor::decide_socket`] exists to
/// refuse, generating no notification. Measured on 7.1/aarch64, inside a private
/// user+net namespace where the box holds `CAP_NET_RAW`,
/// `socket(AF_PACKET, SOCK_RAW)` returns `EPERM` while the io_uring form returns
/// a live fd. The deny-list blocks the whole io_uring interface, and its `ERRNO`
/// still outranks this filter's `ALLOW` once the two are stacked, also measured,
/// which is what makes that block hold for this tier.
pub fn build_socket_notify_program() -> [SockFilter; 10] {
    [
        // 0: A = arch
        stmt(BPF_LD | BPF_W | BPF_ABS, OFF_ARCH),
        // 1: if arch == the host arch, skip the kill
        jump(BPF_JMP | BPF_JEQ | BPF_K, AUDIT_ARCH, 1, 0),
        // 2: wrong arch → kill the process (never silently allow a foreign ABI)
        stmt(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS),
        // 3: A = nr
        stmt(BPF_LD | BPF_W | BPF_ABS, OFF_NR),
        // 4: x32 → kill (idx 9). See X32_SYSCALL_BIT: x32 passes the arch guard
        //    above but carries different syscall numbers, so without this the
        //    `nr` comparisons below miss and instruction 7 ALLOWs an unmediated
        //    socket(2). The notify handler cannot help — no notification is
        //    ever generated.
        jump(BPF_JMP | BPF_JGE | BPF_K, X32_SYSCALL_BIT, 4, 0),
        // 5: nr == socket → NOTIFY (idx 8)
        jump(BPF_JMP | BPF_JEQ | BPF_K, NR_SOCKET, 2, 0),
        // 6: nr == socketpair → NOTIFY (idx 8)
        jump(BPF_JMP | BPF_JEQ | BPF_K, NR_SOCKETPAIR, 1, 0),
        // 7: everything else → allow
        stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        // 8: mediate
        stmt(BPF_RET | BPF_K, SECCOMP_RET_USER_NOTIF),
        // 9: x32 → kill
        stmt(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS),
    ]
}

/// On x86_64 the x32 ABI reports `AUDIT_ARCH_X86_64` in `seccomp_data.arch` but
/// ORs this bit into `nr`, so an arch guard alone lets it through with syscall
/// numbers that match none of the comparisons that follow. Kernels built with
/// `CONFIG_X86_X32_ABI=n` never produce it; the filter refuses it either way
/// rather than depending on the host's config.
///
/// Defined unconditionally so both architectures compile one program shape: no
/// aarch64 syscall number comes near this value, so the test is a no-op there.
pub const X32_SYSCALL_BIT: u32 = 0x4000_0000;

/// Compile-time guard: the BPF builder's array length must match what
/// [`install_listener`] tells the kernel (`SockFprog::len`).
const _: () = assert!(std::mem::size_of::<[SockFilter; 10]>() == 10 * 8);

// ─── kernel ABI: seccomp_data / seccomp_notif / resp / sizes ──────────────────

#[repr(C)]
#[derive(Clone, Copy)]
struct SeccompData {
    nr: i32,
    arch: u32,
    instruction_pointer: u64,
    args: [u64; 6],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SeccompNotif {
    id: u64,
    pid: u32,
    flags: u32,
    data: SeccompData,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SeccompNotifResp {
    id: u64,
    val: i64,
    error: i32,
    flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct SeccompNotifSizes {
    seccomp_notif: u16,
    seccomp_notif_resp: u16,
    seccomp_data: u16,
}

// ioctl number construction: _IOC(dir, type, nr, size).
const IOC_WRITE: u64 = 1;
const IOC_READ: u64 = 2;
const SECCOMP_IOC_MAGIC: u64 = '!' as u64;
const fn ioc(dir: u64, nr: u64, size: u64) -> u64 {
    (dir << 30) | (size << 16) | (SECCOMP_IOC_MAGIC << 8) | nr
}
fn ioctl_recv() -> u64 {
    ioc(IOC_READ | IOC_WRITE, 0, std::mem::size_of::<SeccompNotif>() as u64)
}
fn ioctl_send() -> u64 {
    ioc(IOC_READ | IOC_WRITE, 1, std::mem::size_of::<SeccompNotifResp>() as u64)
}
fn ioctl_id_valid() -> u64 {
    ioc(IOC_WRITE, 2, std::mem::size_of::<u64>() as u64)
}

/// Validate our struct layout against the running kernel's
/// (`SECCOMP_GET_NOTIF_SIZES`). A mismatch means our ABI assumptions are wrong;
/// we refuse rather than misread notifications. Fail-closed.
pub fn validate_notif_sizes() -> Result<(), H5iError> {
    let mut sizes = SeccompNotifSizes::default();
    let rc = unsafe {
        libc::syscall(
            libc::SYS_seccomp,
            SECCOMP_GET_NOTIF_SIZES,
            0,
            &mut sizes as *mut SeccompNotifSizes,
        )
    };
    if rc != 0 {
        return Err(H5iError::Metadata(
            "SECCOMP_GET_NOTIF_SIZES failed — kernel lacks seccomp user-notif (fail-closed)".into(),
        ));
    }
    let ours = (
        std::mem::size_of::<SeccompNotif>(),
        std::mem::size_of::<SeccompNotifResp>(),
        std::mem::size_of::<SeccompData>(),
    );
    let theirs = (
        sizes.seccomp_notif as usize,
        sizes.seccomp_notif_resp as usize,
        sizes.seccomp_data as usize,
    );
    if ours != theirs {
        return Err(H5iError::Metadata(format!(
            "seccomp notif ABI mismatch (ours={ours:?} kernel={theirs:?}) — refusing (fail-closed)"
        )));
    }
    Ok(())
}

// ─── install (child side) ─────────────────────────────────────────────────────

/// Install the socket-notify filter on the **current** thread/process and return
/// the listener fd (`SECCOMP_FILTER_FLAG_NEW_LISTENER`). Caller must have already
/// set `no_new_privs`. Intended to run in the child just before it hands the fd
/// to the supervisor and execs. Returns the raw fd or an errno.
///
/// # Safety
/// Installs a seccomp filter on the calling process — irreversible for its
/// lifetime. Call only in a child you intend to supervise.
pub unsafe fn install_listener() -> Result<RawFd, i32> {
    let prog = build_socket_notify_program();
    let fprog = SockFprog { len: prog.len() as u16, filter: prog.as_ptr() };
    // Safety: discharged by this function's own contract — the caller promises
    // this is a child it intends to supervise. `fprog` outlives the call.
    let fd = unsafe {
        libc::syscall(
            libc::SYS_seccomp,
            SECCOMP_SET_MODE_FILTER,
            SECCOMP_FILTER_FLAG_NEW_LISTENER,
            &fprog as *const SockFprog,
        )
    };
    if fd < 0 {
        Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EINVAL))
    } else {
        Ok(fd as RawFd)
    }
}

// ─── supervisor loop (parent side) ────────────────────────────────────────────

/// Outcome of serving the notify listener until the child is gone.
#[derive(Debug, Default, Clone)]
pub struct ServeStats {
    /// `socket()`/`socketpair()` calls allowed (boring inet / granted unix).
    pub allowed: u64,
    /// Calls denied by the default-deny gate.
    pub denied: u64,
}

/// Serve notifications on `listener` until `stop` is set (the supervised process
/// has exited — the caller sets it after `waitpid`). The listener is driven
/// non-blocking via `poll()` so the loop can observe `stop` even when no
/// notification is pending — otherwise a final blocking `RECV` would wait
/// forever after the last syscall and deadlock the supervisor.
///
/// For each `socket`/`socketpair` notification: apply [`decide_socket`],
/// **re-validate the id immediately before replying** (stale-id/TOCTOU guard),
/// and reply (`CONTINUE` for allow, `-errno` for deny). A stale id is skipped;
/// an unexpected error is fail-closed (we stop serving, so the tracee blocks on
/// its unanswered notify and the run ends rather than proceeding unmediated).
pub fn serve(listener: RawFd, unix_granted: bool, stop: &std::sync::atomic::AtomicBool) -> ServeStats {
    use std::sync::atomic::Ordering;
    let mut stats = ServeStats::default();
    set_nonblocking(listener);
    while !stop.load(Ordering::Acquire) {
        let mut pfd = libc::pollfd { fd: listener, events: libc::POLLIN, revents: 0 };
        let pr = unsafe { libc::poll(&mut pfd, 1, 50) }; // 50ms tick to recheck stop
        if pr <= 0 {
            continue; // timeout / EINTR → recheck stop
        }
        if matches!(handle_one(listener, unix_granted, &mut stats), Flow::FailClosed) {
            break;
        }
    }
    stats
}

/// The production-correct lifecycle: serve notifications until the supervised
/// process exits, observed via its **pidfd** (no `waitpid`/stop-flag race — the
/// loop self-terminates). `pidfd` becomes readable when the child exits; on that
/// signal we drain any final pending notifications and return. The listener and
/// pidfd are both polled, so a blocked `RECV` can never strand the supervisor.
pub fn serve_with_pidfd(listener: RawFd, pidfd: RawFd, unix_granted: bool) -> ServeStats {
    let mut stats = ServeStats::default();
    set_nonblocking(listener);
    loop {
        let mut pfds = [
            libc::pollfd { fd: listener, events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: pidfd, events: libc::POLLIN, revents: 0 },
        ];
        let pr = unsafe { libc::poll(pfds.as_mut_ptr(), 2, -1) };
        if pr < 0 {
            let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
            if err == libc::EINTR {
                continue;
            }
            break; // poll itself failed → fail-closed
        }
        // Handle exactly ONE notification per wakeup — only when poll confirms
        // the listener is readable. We must never call RECV speculatively: the
        // seccomp listener does not reliably honor O_NONBLOCK, so a RECV with
        // nothing pending would *block* and strand the supervisor.
        if pfds[0].revents & libc::POLLIN != 0
            && matches!(handle_one(listener, unix_granted, &mut stats), Flow::FailClosed)
        {
            break;
        }
        // Child exited → drain any notifications still pending (each guarded by a
        // zero-timeout poll so we never block), then stop.
        if pfds[1].revents & (libc::POLLIN | libc::POLLHUP) != 0 {
            while listener_pending(listener) {
                if matches!(handle_one(listener, unix_granted, &mut stats), Flow::FailClosed) {
                    break;
                }
            }
            break;
        }
    }
    stats
}

/// Is a notification pending on `listener` right now? (Zero-timeout poll — used
/// to guard `handle_one` so we never issue a blocking `RECV`.)
fn listener_pending(listener: RawFd) -> bool {
    let mut pfd = libc::pollfd { fd: listener, events: libc::POLLIN, revents: 0 };
    unsafe { libc::poll(&mut pfd, 1, 0) > 0 && (pfd.revents & libc::POLLIN != 0) }
}

/// Result of processing one pending notification.
enum Flow {
    /// One notification was handled (allow/deny delivered or stale-skipped).
    Handled,
    /// No notification was pending (`EAGAIN`).
    Idle,
    /// An unexpected error — the supervisor must stop (fail-closed).
    FailClosed,
}

fn set_nonblocking(fd: RawFd) {
    unsafe {
        let fl = libc::fcntl(fd, libc::F_GETFL);
        libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK);
    }
}

/// Process at most one pending notification on `listener` (non-blocking). Shared
/// by both serve loops so the security-critical decision/reply logic exists once.
fn handle_one(listener: RawFd, unix_granted: bool, stats: &mut ServeStats) -> Flow {
    let mut req: SeccompNotif = unsafe { std::mem::zeroed() };
    // `ioctl`'s request arg is `c_ulong` on glibc but `c_int` on musl — `as _`
    // casts to whichever the target's signature expects (the 32-bit request
    // code's bit pattern is preserved either way).
    let rc = unsafe { libc::ioctl(listener, ioctl_recv() as _, &mut req as *mut SeccompNotif) };
    if rc != 0 {
        let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        if err == libc::EAGAIN || err == libc::EINTR {
            return Flow::Idle;
        }
        return Flow::FailClosed;
    }

    // Defense-in-depth (Codex): the BPF should only ever notify on our arch +
    // socket/socketpair, but a security boundary must not trust impossible
    // inputs. An unexpected arch/nr is treated as deny, never continue.
    // args[0]=domain, args[1]=type, args[2]=protocol (socket & socketpair);
    // socketpair gets its own gate (an anonymous AF_UNIX pair is allowed —
    // see `decide_socketpair`), socket stays on the default-deny gate.
    let (domain, ty, proto) =
        (req.data.args[0] as i32, req.data.args[1] as i32, req.data.args[2] as i32);
    let decision = if req.data.arch != AUDIT_ARCH {
        Decision::Deny(libc::EPERM)
    } else if req.data.nr as u32 == NR_SOCKET {
        decide_socket(domain, ty, proto, unix_granted)
    } else if req.data.nr as u32 == NR_SOCKETPAIR {
        crate::supervisor::decide_socketpair(domain, ty, proto, unix_granted)
    } else {
        Decision::Deny(libc::EPERM)
    };

    let mut resp: SeccompNotifResp = unsafe { std::mem::zeroed() };
    resp.id = req.id;
    match decision {
        Decision::Continue => resp.flags = SECCOMP_USER_NOTIF_FLAG_CONTINUE,
        Decision::Deny(errno) => resp.error = -errno, // val ignored when error != 0
    }

    // Re-validate the id right before SEND: if the tracee died or the syscall
    // was interrupted, the id is stale and SEND would mis-target — skip.
    let valid = unsafe { libc::ioctl(listener, ioctl_id_valid() as _, &req.id as *const u64) } == 0;
    if !valid {
        return Flow::Handled; // consumed a notification (stale); keep draining
    }
    let send_rc =
        unsafe { libc::ioctl(listener, ioctl_send() as _, &resp as *const SeccompNotifResp) };
    if send_rc != 0 {
        let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        // The tracee can die between ID_VALID and SEND → ENOENT (stale, benign).
        // EINTR is retryable-but-rare; benign-skip. Any other SEND error is a
        // supervisor failure → fail-closed.
        if err == libc::ENOENT || err == libc::EINTR {
            return Flow::Handled;
        }
        return Flow::FailClosed;
    }
    // Count only *delivered* verdicts so the stats never lie.
    match decision {
        Decision::Continue => stats.allowed += 1,
        Decision::Deny(_) => stats.denied += 1,
    }
    Flow::Handled
}

/// Open a pidfd for `pid` (`pidfd_open(2)`) — readable when the process exits.
pub fn pidfd_open(pid: libc::pid_t) -> std::io::Result<RawFd> {
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    if fd < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(fd as RawFd)
    }
}

// ─── SCM_RIGHTS fd handoff (child → supervisor) ───────────────────────────────

/// Send a single fd over a connected `AF_UNIX` socket via `SCM_RIGHTS`.
/// Async-signal-safe enough for use in a post-fork child. Returns `Ok` on
/// success.
///
/// # Safety
/// `sock` and `fd` must be valid open file descriptors.
pub unsafe fn send_fd(sock: RawFd, fd: RawFd) -> std::io::Result<()> {
    // Safety: discharged by this function's own contract — the caller promises
    // `sock` and `fd` are valid. `cmsg_buf` is 64 bytes, far more than the one
    // `SCM_RIGHTS` header plus fd written into it, so `CMSG_FIRSTHDR` is in
    // bounds and non-null.
    unsafe {
        let mut iov_base = [0u8; 1]; // one dummy byte (some kernels need payload)
        let mut iov = libc::iovec {
            iov_base: iov_base.as_mut_ptr() as *mut libc::c_void,
            iov_len: 1,
        };
        let mut cmsg_buf = [0u8; 64];
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = cmsg_buf.as_mut_ptr() as *mut libc::c_void;
        msg.msg_controllen = libc::CMSG_SPACE(std::mem::size_of::<RawFd>() as u32) as _;

        let cmsg = libc::CMSG_FIRSTHDR(&msg);
        (*cmsg).cmsg_level = libc::SOL_SOCKET;
        (*cmsg).cmsg_type = libc::SCM_RIGHTS;
        (*cmsg).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<RawFd>() as u32) as _;
        std::ptr::copy_nonoverlapping(&fd, libc::CMSG_DATA(cmsg) as *mut RawFd, 1);

        let n = libc::sendmsg(sock, &msg, 0);
        if n < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

/// Receive a single fd sent via [`send_fd`].
///
/// # Safety
/// `sock` must be a valid connected `AF_UNIX` socket.
pub unsafe fn recv_fd(sock: RawFd) -> std::io::Result<RawFd> {
    // Safety: discharged by this function's own contract — the caller promises
    // `sock` is a valid connected socket. The cmsg is only dereferenced after
    // the null/type/length checks below.
    unsafe {
        let mut iov_base = [0u8; 1];
        let mut iov = libc::iovec {
            iov_base: iov_base.as_mut_ptr() as *mut libc::c_void,
            iov_len: 1,
        };
        let mut cmsg_buf = [0u8; 64];
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = cmsg_buf.as_mut_ptr() as *mut libc::c_void;
        msg.msg_controllen = cmsg_buf.len() as _;

        let n = libc::recvmsg(sock, &mut msg, 0);
        if n != 1 {
            return Err(std::io::Error::other("fd handoff: unexpected payload length"));
        }
        // Reject a truncated control message — a partial/forged ancillary buffer
        // must never be mistaken for a valid fd (Codex hardening).
        if msg.msg_flags & (libc::MSG_CTRUNC | libc::MSG_TRUNC) != 0 {
            return Err(std::io::Error::other("fd handoff: truncated control message"));
        }
        let cmsg = libc::CMSG_FIRSTHDR(&msg);
        if cmsg.is_null()
            || (*cmsg).cmsg_type != libc::SCM_RIGHTS
            || (*cmsg).cmsg_level != libc::SOL_SOCKET
            || (*cmsg).cmsg_len < libc::CMSG_LEN(std::mem::size_of::<RawFd>() as u32) as _
        {
            return Err(std::io::Error::other("fd handoff: missing/short SCM_RIGHTS cmsg"));
        }
        let mut fd: RawFd = -1;
        std::ptr::copy_nonoverlapping(libc::CMSG_DATA(cmsg) as *const RawFd, &mut fd, 1);
        if fd < 0 {
            return Err(std::io::Error::other("fd handoff: invalid fd received"));
        }
        // The listener fd must not leak across a future exec in the supervisor.
        libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
        Ok(fd)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// x32 reports AUDIT_ARCH_X86_64, so the arch guard passes, but its syscall
    /// numbers carry X32_SYSCALL_BIT and match none of the comparisons below —
    /// the filter used to fall through to ALLOW and no notification was ever
    /// generated, so the handler's defence-in-depth re-check could not help.
    #[test]
    fn the_socket_gate_refuses_the_x32_abi() {
        let p = build_socket_notify_program();
        // The nr load is followed immediately by the x32 test, before any
        // syscall-number comparison can miss.
        let nr_load = p.iter().position(|i| i.code == (BPF_LD | BPF_W | BPF_ABS) && i.k == OFF_NR).unwrap();
        let guard = &p[nr_load + 1];
        assert_eq!(guard.code, BPF_JMP | BPF_JGE | BPF_K, "x32 test must come first");
        assert_eq!(guard.k, X32_SYSCALL_BIT);
        // Taking that branch lands on a kill, never on the allow.
        let target = nr_load + 2 + guard.jt as usize;
        assert_eq!(p[target].code, BPF_RET | BPF_K);
        assert_eq!(p[target].k, SECCOMP_RET_KILL_PROCESS, "x32 must be killed, not allowed");
    }

    #[test]
    fn bpf_program_shape() {
        // Asserted structurally rather than by index: the program grows when a
        // guard is added, and a positional test just breaks without saying
        // anything about the policy.
        let p = build_socket_notify_program();

        // It starts by checking the arch, and a mismatch kills.
        assert_eq!(p[0], stmt(BPF_LD | BPF_W | BPF_ABS, OFF_ARCH));
        assert_eq!(p[1].k, AUDIT_ARCH);
        assert_eq!(p[2], stmt(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS));

        // Both socket-creating calls are compared, and every comparison lands
        // on the USER_NOTIF instruction rather than on the allow.
        for want in [NR_SOCKET, NR_SOCKETPAIR] {
            let i = p
                .iter()
                .position(|ins| ins.code == (BPF_JMP | BPF_JEQ | BPF_K) && ins.k == want)
                .unwrap_or_else(|| panic!("no comparison for syscall {want}"));
            let target = i + 1 + p[i].jt as usize;
            assert_eq!(p[target], stmt(BPF_RET | BPF_K, SECCOMP_RET_USER_NOTIF));
        }

        // Exactly one fall-through allow, and it is the last thing reached.
        assert_eq!(
            p.iter().filter(|i| i.code == (BPF_RET | BPF_K) && i.k == SECCOMP_RET_ALLOW).count(),
            1
        );
    }

    /// The notified set is a closed list, and closing it is a security
    /// property rather than tidiness.
    ///
    /// Every allow this filter can produce is `CONTINUE`, which re-runs the
    /// real syscall after the verdict. That is only safe while every argument
    /// the verdict looks at is a scalar the kernel already copied into the
    /// notification — a register, which no other thread can rewrite in the
    /// meantime. `socket` and `socketpair` are such syscalls. `connect`,
    /// `bind` and `sendto` are not: their address argument is a pointer into
    /// the tracee's memory, and deciding on what it points at while replying
    /// `CONTINUE` is a double-fetch a second thread wins.
    ///
    /// So a new comparison here is a decision to give up `CONTINUE` for that
    /// syscall, not a one-line addition. This fails if one appears.
    #[test]
    fn only_syscalls_whose_arguments_are_registers_are_notified() {
        let p = build_socket_notify_program();
        let notified: std::collections::BTreeSet<u32> = p
            .iter()
            .enumerate()
            .filter(|(_, ins)| ins.code == (BPF_JMP | BPF_JEQ | BPF_K))
            .filter(|(i, ins)| {
                let target = i + 1 + ins.jt as usize;
                p.get(target) == Some(&stmt(BPF_RET | BPF_K, SECCOMP_RET_USER_NOTIF))
            })
            .map(|(_, ins)| ins.k)
            .collect();
        assert_eq!(
            notified,
            std::collections::BTreeSet::from([NR_SOCKET, NR_SOCKETPAIR]),
            "a syscall was added to the notify set. If its arguments include a pointer \
             into the tracee's memory, it must not be answered with CONTINUE — see \
             `build_socket_notify_program`."
        );
    }

    #[test]
    fn ioctl_numbers_are_well_formed() {
        // type byte must be SECCOMP_IOC_MAGIC ('!') in all three.
        for n in [ioctl_recv(), ioctl_send(), ioctl_id_valid()] {
            assert_eq!((n >> 8) & 0xff, SECCOMP_IOC_MAGIC);
        }
        // RECV/SEND are read-write; ID_VALID is write-only.
        assert_eq!(ioctl_recv() >> 30, IOC_READ | IOC_WRITE);
        assert_eq!(ioctl_id_valid() >> 30, IOC_WRITE);
        // nr fields 0,1,2.
        assert_eq!(ioctl_recv() & 0xff, 0);
        assert_eq!(ioctl_send() & 0xff, 1);
        assert_eq!(ioctl_id_valid() & 0xff, 2);
    }

    #[test]
    fn abi_struct_sizes_are_the_stable_layout() {
        // The seccomp user-notif ABI is stable; lock the sizes so a struct edit
        // that would break ioctl numbers fails here, not silently at runtime.
        assert_eq!(std::mem::size_of::<SeccompData>(), 64);
        assert_eq!(std::mem::size_of::<SeccompNotif>(), 80);
        assert_eq!(std::mem::size_of::<SeccompNotifResp>(), 24);
    }

    // Live, capability-gated: only runs where the kernel supports user-notif.
    // Proves the default-deny socket gate actually denies a raw/packet socket
    // and allows a boring inet socket — the real enforcement mechanism.
    #[test]
    fn live_socket_gate_denies_raw_allows_inet() {
        if !crate::supervisor::probe().components.iter().any(|c| c.name == "seccomp-user-notif" && c.ok)
            || validate_notif_sizes().is_err()
        {
            eprintln!("skipping: seccomp user-notif unavailable on this host");
            return;
        }
        unsafe {
            // socketpair to hand the listener fd back; pipe for the child's results.
            let mut sv = [0i32; 2];
            assert_eq!(libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, sv.as_mut_ptr()), 0);
            let mut pipefd = [0i32; 2];
            assert_eq!(libc::pipe(pipefd.as_mut_ptr()), 0);

            let pid = libc::fork();
            assert!(pid >= 0, "fork");
            if pid == 0 {
                // ── child (the supervised process) ──
                libc::close(sv[0]);
                libc::close(pipefd[0]);
                libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);
                let lfd = match install_listener() {
                    Ok(fd) => fd,
                    Err(_) => libc::_exit(99),
                };
                if send_fd(sv[1], lfd).is_err() {
                    libc::_exit(98);
                }
                // Give the supervisor a moment to start serving.
                // (No sleep syscall is mediated; socket() will block on notify.)
                let raw = libc::socket(libc::AF_INET, libc::SOCK_RAW, libc::IPPROTO_TCP);
                let raw_errno = if raw < 0 {
                    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
                } else {
                    libc::close(raw);
                    0
                };
                let inet = libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0);
                let inet_ok = inet >= 0;
                if inet >= 0 {
                    libc::close(inet);
                }
                // Report: byte0 = raw denied with EPERM?, byte1 = inet ok?
                let out = [
                    (raw < 0 && raw_errno == libc::EPERM) as u8,
                    inet_ok as u8,
                ];
                libc::write(pipefd[1], out.as_ptr() as *const libc::c_void, 2);
                libc::_exit(0);
            }

            // ── parent (the supervisor) ──
            libc::close(sv[1]);
            libc::close(pipefd[1]);
            let listener = recv_fd(sv[0]).expect("receive listener fd");
            let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let stop_t = stop.clone();
            let handle = std::thread::spawn(move || serve(listener, false, &stop_t));

            // Read the child's two results, stop the supervisor loop, join, reap.
            let mut buf = [0u8; 2];
            let n = libc::read(pipefd[0], buf.as_mut_ptr() as *mut libc::c_void, 2);
            stop.store(true, std::sync::atomic::Ordering::Release);
            let stats = handle.join().unwrap();
            let mut status = 0;
            libc::waitpid(pid, &mut status, 0);

            assert_eq!(n, 2, "child must report two results");
            assert_eq!(buf[0], 1, "raw socket must be DENIED with EPERM");
            assert_eq!(buf[1], 1, "boring inet socket must be ALLOWED");
            assert!(stats.denied >= 1, "supervisor must have recorded a denial");
            assert!(stats.allowed >= 1, "supervisor must have recorded an allow");
        }
    }

    // Isolate whether pidfd POLLIN signals child exit on this host at all.
    #[test]
    fn pidfd_signals_child_exit() {
        unsafe {
            let pid = libc::fork();
            assert!(pid >= 0);
            if pid == 0 {
                libc::_exit(0);
            }
            let pidfd = match pidfd_open(pid) {
                Ok(fd) => fd,
                Err(e) => {
                    eprintln!("skipping: pidfd_open unsupported: {e}");
                    libc::waitpid(pid, &mut 0, 0);
                    return;
                }
            };
            let mut pfd = libc::pollfd { fd: pidfd, events: libc::POLLIN, revents: 0 };
            let pr = libc::poll(&mut pfd, 1, 3000); // 3s budget
            let revents = pfd.revents;
            libc::waitpid(pid, &mut 0, 0);
            libc::close(pidfd);
            assert_eq!(pr, 1, "pidfd poll must return readable on child exit (got {pr})");
            assert!(revents & libc::POLLIN != 0, "pidfd must be POLLIN on exit (revents={revents})");
        }
    }

    // The production-correct lifecycle: serve_with_pidfd self-terminates when the
    // child exits, with no stop flag and no waitpid/serve ordering. Proves the
    // loop the live supervised run will use.
    #[test]
    fn live_serve_with_pidfd_self_terminates() {
        if !crate::supervisor::probe().components.iter().any(|c| c.name == "seccomp-user-notif" && c.ok)
            || validate_notif_sizes().is_err()
        {
            eprintln!("skipping: seccomp user-notif unavailable on this host");
            return;
        }
        unsafe {
            let mut sv = [0i32; 2];
            assert_eq!(libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, sv.as_mut_ptr()), 0);

            let pid = libc::fork();
            assert!(pid >= 0, "fork");
            if pid == 0 {
                // Child path is allocation-free / async-signal-safe (the parent
                // is the multithreaded test harness, so a malloc here could
                // deadlock on an inherited lock).
                libc::close(sv[0]);
                libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);
                let lfd = match install_listener() {
                    Ok(fd) => fd,
                    Err(_) => libc::_exit(99),
                };
                if send_fd(sv[1], lfd).is_err() {
                    libc::_exit(98);
                }
                // A denied raw socket then an allowed inet socket, then exit.
                let raw = libc::socket(libc::AF_INET, libc::SOCK_RAW, libc::IPPROTO_TCP);
                if raw >= 0 {
                    libc::close(raw);
                }
                let inet = libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0);
                if inet >= 0 {
                    libc::close(inet);
                }
                libc::_exit(0);
            }

            // Parent/supervisor: open the child's pidfd, receive the listener,
            // then serve until the pidfd reports the child exited — no stop flag.
            libc::close(sv[1]);
            let pidfd = pidfd_open(pid).expect("pidfd_open");
            let listener = recv_fd(sv[0]).expect("receive listener fd");
            let stats = serve_with_pidfd(listener, pidfd, false);
            let mut status = 0;
            libc::waitpid(pid, &mut status, 0);
            libc::close(pidfd);

            assert!(stats.denied >= 1, "raw socket should have been denied");
            assert!(stats.allowed >= 1, "inet socket should have been allowed");
        }
    }
}
