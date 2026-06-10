//! seccomp user-notification primitives for the supervisor tier
//! (`docs/supervisor-design.md`, phase B).
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
/// else, and KILLs on an unexpected architecture (fail-closed). Pure — the BPF
/// is structurally unit-tested.
pub fn build_socket_notify_program() -> Vec<SockFilter> {
    vec![
        // 0: A = arch
        stmt(BPF_LD | BPF_W | BPF_ABS, OFF_ARCH),
        // 1: if arch == the host arch, skip the kill
        jump(BPF_JMP | BPF_JEQ | BPF_K, AUDIT_ARCH, 1, 0),
        // 2: wrong arch → kill the process (never silently allow a foreign ABI)
        stmt(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS),
        // 3: A = nr
        stmt(BPF_LD | BPF_W | BPF_ABS, OFF_NR),
        // 4: nr == socket → NOTIFY (idx 7)
        jump(BPF_JMP | BPF_JEQ | BPF_K, NR_SOCKET, 2, 0),
        // 5: nr == socketpair → NOTIFY (idx 7)
        jump(BPF_JMP | BPF_JEQ | BPF_K, NR_SOCKETPAIR, 1, 0),
        // 6: everything else → allow
        stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        // 7: mediate
        stmt(BPF_RET | BPF_K, SECCOMP_RET_USER_NOTIF),
    ]
}

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
    let fd = libc::syscall(
        libc::SYS_seccomp,
        SECCOMP_SET_MODE_FILTER,
        SECCOMP_FILTER_FLAG_NEW_LISTENER,
        &fprog as *const SockFprog,
    );
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
    // Non-blocking so poll() governs blocking and we can check `stop`.
    unsafe {
        let fl = libc::fcntl(listener, libc::F_GETFL);
        libc::fcntl(listener, libc::F_SETFL, fl | libc::O_NONBLOCK);
    }
    while !stop.load(Ordering::Acquire) {
        let mut pfd = libc::pollfd { fd: listener, events: libc::POLLIN, revents: 0 };
        let pr = unsafe { libc::poll(&mut pfd, 1, 50) }; // 50ms tick to recheck stop
        if pr <= 0 {
            continue; // timeout / EINTR → recheck stop
        }

        let mut req: SeccompNotif = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::ioctl(listener, ioctl_recv(), &mut req as *mut SeccompNotif) };
        if rc != 0 {
            let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
            if err == libc::EAGAIN || err == libc::EINTR {
                continue;
            }
            break; // a real error → fail-closed
        }

        // Defense-in-depth (Codex): the BPF should only ever notify on our arch
        // + socket/socketpair, but a security boundary must not trust impossible
        // inputs. An unexpected arch/nr is treated as deny, never continue.
        let trusted_shape = req.data.arch == AUDIT_ARCH
            && (req.data.nr as u32 == NR_SOCKET || req.data.nr as u32 == NR_SOCKETPAIR);
        let decision = if trusted_shape {
            // args[0]=domain, args[1]=type, args[2]=protocol (socket & socketpair).
            decide_socket(
                req.data.args[0] as i32,
                req.data.args[1] as i32,
                req.data.args[2] as i32,
                unix_granted,
            )
        } else {
            Decision::Deny(libc::EPERM)
        };

        let mut resp: SeccompNotifResp = unsafe { std::mem::zeroed() };
        resp.id = req.id;
        match decision {
            Decision::Continue => resp.flags = SECCOMP_USER_NOTIF_FLAG_CONTINUE,
            Decision::Deny(errno) => resp.error = -errno, // val ignored when error != 0
        }

        // Re-validate the id right before SEND: if the tracee died or the
        // syscall was interrupted, the id is stale and SEND would mis-target —
        // skip (never reply to a stale id).
        let valid = unsafe { libc::ioctl(listener, ioctl_id_valid(), &req.id as *const u64) } == 0;
        if !valid {
            continue;
        }
        let send_rc = unsafe { libc::ioctl(listener, ioctl_send(), &resp as *const SeccompNotifResp) };
        if send_rc != 0 {
            let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
            // The tracee can die between ID_VALID and SEND → ENOENT (stale,
            // benign). EINTR is retryable-but-rare; treat as benign-skip. Any
            // other SEND error is a supervisor failure → fail-closed.
            if err == libc::ENOENT || err == libc::EINTR {
                continue;
            }
            break;
        }
        // Count only *delivered* verdicts so the stats never lie.
        match decision {
            Decision::Continue => stats.allowed += 1,
            Decision::Deny(_) => stats.denied += 1,
        }
    }
    stats
}

// ─── SCM_RIGHTS fd handoff (child → supervisor) ───────────────────────────────

/// Send a single fd over a connected `AF_UNIX` socket via `SCM_RIGHTS`.
/// Async-signal-safe enough for use in a post-fork child. Returns `Ok` on
/// success.
///
/// # Safety
/// `sock` and `fd` must be valid open file descriptors.
pub unsafe fn send_fd(sock: RawFd, fd: RawFd) -> std::io::Result<()> {
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

/// Receive a single fd sent via [`send_fd`].
///
/// # Safety
/// `sock` must be a valid connected `AF_UNIX` socket.
pub unsafe fn recv_fd(sock: RawFd) -> std::io::Result<RawFd> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bpf_program_shape() {
        let p = build_socket_notify_program();
        assert_eq!(p.len(), 8);
        // Last two are ALLOW then USER_NOTIF.
        assert_eq!(p[6], stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW));
        assert_eq!(p[7], stmt(BPF_RET | BPF_K, SECCOMP_RET_USER_NOTIF));
        // Arch guard kills on mismatch.
        assert_eq!(p[2], stmt(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS));
        // socket / socketpair compares jump forward to the NOTIFY instruction.
        assert_eq!(p[4].k, NR_SOCKET);
        assert_eq!(p[5].k, NR_SOCKETPAIR);
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
}
