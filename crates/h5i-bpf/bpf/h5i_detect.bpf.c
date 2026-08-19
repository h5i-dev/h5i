/* SPDX-License-Identifier: (BSD-2-Clause OR GPL-2.0)
 *
 * h5i_detect.bpf.c — the kernel half of h5i's runtime detection lane
 * (ROADMAP.md D1–D14).
 *
 * Twelve syscall/scheduler tracepoints, one fixed-size event, one ring
 * buffer. It reads no kernel structure, calls no unstable helper, and needs
 * neither BTF nor vmlinux.h at build time or at run time (D5).
 *
 * It cannot block anything. There is no `bpf_send_signal`, no
 * `bpf_override_return`, and no LSM program in this file, and that is a
 * design decision rather than an unfinished one: enforcement in h5i lives in
 * the mechanisms that fail closed by construction — Landlock, seccomp, the
 * network namespace, the egress proxy — and a second thing that sometimes
 * denies would blur a boundary that is currently sharp (D12).
 *
 * Licence: the object is "Dual BSD/GPL" because several helpers it calls are
 * GPL-only in the kernel's view (`bpf_probe_read_user_str` among them), and a
 * BSD-only object would simply be refused at load. The repository is Apache
 * 2.0; this file carries the dual notice so both the kernel's requirement and
 * the project's licence are satisfied for the one artifact that needs it.
 */

#include "h5i_bpf.h"
#include "h5i_event.h"

char _license[] SEC("license") = "Dual BSD/GPL";

/* ------------------------------------------------------------------ maps */

struct {
    __uint(type, BPF_MAP_TYPE_RINGBUF);
    /* Overridden by the loader from the profile's `buffer_kb`; this default
     * is the one a `cargo build` was sized against. */
    __uint(max_entries, 262144);
} H5I_EVENTS SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, 16384);
    __type(key, __u32);
    __type(value, __u8);
} H5I_TRACKED SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(max_entries, 1);
    __type(key, __u32);
    __type(value, struct h5i_config);
} H5I_CONFIG SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(max_entries, H5I_MAX_PREFIX);
    __type(key, __u32);
    __type(value, struct h5i_prefix);
} H5I_PREFIXES SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_PERCPU_ARRAY);
    __uint(max_entries, H5I_STAT_MAX);
    __type(key, __u32);
    __type(value, __u64);
} H5I_STATS SEC(".maps");

/* --------------------------------------------------------------- helpers */

static __always_inline void h5i_bump(__u32 slot)
{
    __u64 *v = bpf_map_lookup_elem(&H5I_STATS, &slot);
    if (v)
        __sync_fetch_and_add(v, 1);
}

static __always_inline struct h5i_config *h5i_cfg(void)
{
    __u32 zero = 0;
    return bpf_map_lookup_elem(&H5I_CONFIG, &zero);
}

/* Resolve this task's place in the box's process tree, promoting a PENDING
 * task on the way. See the state machine in h5i_event.h. */
static __always_inline __u8 h5i_state(void)
{
    __u64 pt = bpf_get_current_pid_tgid();
    __u32 tid = (__u32)pt;
    __u32 tgid = (__u32)(pt >> 32);

    __u8 *st = bpf_map_lookup_elem(&H5I_TRACKED, &tid);
    if (!st)
        return H5I_ST_NONE;
    __u8 v = *st;
    if (v != H5I_ST_PEND)
        return v;

    __u8 next = (tid == tgid) ? H5I_ST_PRE : H5I_ST_SELF;
    bpf_map_update_elem(&H5I_TRACKED, &tid, &next, BPF_ANY);
    return next;
}

static __always_inline int h5i_kind_on(__u16 kind)
{
    struct h5i_config *cfg = h5i_cfg();
    if (!cfg)
        return 0;
    return (cfg->kind_mask >> kind) & 1;
}

/* The gate every program runs first: is this task in scope, is this kind
 * enabled, and is the task far enough along to be attributed to the box?
 *
 * Returns the resolved state, or H5I_ST_NONE when nothing should be emitted.
 */
static __always_inline __u8 h5i_admit(__u16 kind)
{
    __u8 st = h5i_state();
    if (st != H5I_ST_LIVE && st != H5I_ST_PRE)
        return H5I_ST_NONE;
    /* Before its exec, a task is still running h5i's own bootstrap. Only the
     * exec itself and the tree bookkeeping are the box's. */
    if (st == H5I_ST_PRE && kind != H5I_KIND_EXEC && kind != H5I_KIND_FORK &&
        kind != H5I_KIND_EXIT)
        return H5I_ST_NONE;
    if (!h5i_kind_on(kind))
        return H5I_ST_NONE;
    return st;
}

/* Reserve and pre-fill an event. Zeroed in full: the ring buffer hands back
 * whatever was in the page, and the string fields are copied to userspace
 * wholesale, so anything short of a full clear is a kernel memory disclosure
 * across the very boundary this code exists to watch. */
static __always_inline struct h5i_event *h5i_begin(__u16 kind)
{
    struct h5i_event *e = bpf_ringbuf_reserve(&H5I_EVENTS, sizeof(*e), 0);
    if (!e) {
        h5i_bump(H5I_STAT_LOST);
        return 0;
    }
    __builtin_memset(e, 0, sizeof(*e));
    e->magic = H5I_EVENT_MAGIC;
    e->version = H5I_EVENT_VERSION;
    e->kind = kind;
    e->ts_ns = bpf_ktime_get_ns();

    __u64 pt = bpf_get_current_pid_tgid();
    e->tid = (__u32)pt;
    e->tgid = (__u32)(pt >> 32);
    e->uid = (__u32)bpf_get_current_uid_gid();
    bpf_get_current_comm(&e->comm, sizeof(e->comm));
    return e;
}

static __always_inline void h5i_emit(struct h5i_event *e)
{
    h5i_bump(H5I_STAT_EMITTED);
    bpf_ringbuf_submit(e, 0);
}

static __always_inline void h5i_drop(struct h5i_event *e)
{
    h5i_bump(H5I_STAT_FILTERED);
    bpf_ringbuf_discard(e, 0);
}

/* Fully unrolled, constant-index prefix match.
 *
 * Neither loop has an early exit, and that is the point rather than an
 * oversight: clang refuses to unroll a loop with more than one exit, and an
 * un-unrolled version leaves the verifier reasoning about a variable index
 * into a ring-buffer field. Written this way both bounds are compile-time
 * constants, every index into `path` and into the prefix record is a literal
 * after unrolling, and the whole filter is straight-line code with nothing
 * for the verifier to disagree about. The cost is that a match still walks
 * the remaining prefixes; at twelve of them that is not a cost worth an
 * argument.
 */
static __always_inline int h5i_prefix_hit(const char *path)
{
    struct h5i_config *cfg = h5i_cfg();
    if (!cfg)
        return 0;
    __u32 n = cfg->prefix_count;
    if (n > H5I_MAX_PREFIX)
        n = H5I_MAX_PREFIX;

    int hit = 0;
#pragma unroll
    for (__u32 i = 0; i < H5I_MAX_PREFIX; i++) {
        if (i < n) {
            __u32 idx = i;
            struct h5i_prefix *pf = bpf_map_lookup_elem(&H5I_PREFIXES, &idx);
            if (pf) {
                __u32 len = pf->len;
                if (len > 0 && len <= H5I_PREFIX_LEN) {
                    /* Eight-byte head test first. The full comparison is
                     * still compiled — it has to be, the unroll is the whole
                     * point — but at run time almost every path fails here
                     * and branches over it, which is the difference between
                     * a few hundred instructions per `openat` and a few
                     * thousand. */
                    int head = 1;
                    if (len >= 8) {
#pragma unroll
                        for (__u32 h = 0; h < 8; h++) {
                            if (path[h] != pf->s[h])
                                head = 0;
                        }
                    }
                    if (head) {
                        int ok = 1;
#pragma unroll
                        for (__u32 j = 0; j < H5I_PREFIX_LEN; j++) {
                            if (j < len && path[j] != pf->s[j])
                                ok = 0;
                        }
                        if (ok)
                            hit = 1;
                    }
                }
            }
        }
    }
    return hit;
}

/* Does the path contain `/.env`?
 *
 * The one filter that needs substring semantics, so it is a scan rather than
 * a map lookup (see `want_dotenv` in h5i_event.h). Unrolled and exit-free
 * like everything else here; the `done` flag stops the comparisons at the
 * terminating NUL, so the run-time cost is the length of the path rather than
 * the size of the buffer.
 */
static __always_inline int h5i_dotenv_hit(const char *path)
{
    int done = 0;
    int hit = 0;
#pragma unroll
    for (__u32 i = 0; i + 5 <= H5I_PATH_LEN; i++) {
        if (!done) {
            if (path[i] == 0)
                done = 1;
            else if (path[i] == '/' && path[i + 1] == '.' && path[i + 2] == 'e' &&
                     path[i + 3] == 'n' && path[i + 4] == 'v')
                hit = 1;
        }
    }
    return hit;
}

/* Write intent, as the open flags spell it. The five bits are identical on
 * every architecture h5i builds for. */
#define H5I_O_WRONLY 00000001
#define H5I_O_RDWR 00000002
#define H5I_O_CREAT 00000100
#define H5I_O_TRUNC 00001000
#define H5I_O_APPEND 00002000
#define H5I_O_WRITEISH \
    (H5I_O_WRONLY | H5I_O_RDWR | H5I_O_CREAT | H5I_O_TRUNC | H5I_O_APPEND)

#define H5I_AF_UNIX 1
#define H5I_AF_INET 2
#define H5I_AF_INET6 10

/* ---------------------------------------------------------------- events */

/* Promote this task past its bootstrap. Called on every exec, whether or not
 * the event itself was emitted: the state machine is not the kind mask's
 * business, and a run with `Exec` disabled must still attribute what follows
 * to the box. */
static __always_inline void h5i_mark_execed(void)
{
    __u64 pt = bpf_get_current_pid_tgid();
    __u32 tid = (__u32)pt;
    __u8 *st = bpf_map_lookup_elem(&H5I_TRACKED, &tid);
    if (st && (*st == H5I_ST_PRE || *st == H5I_ST_LIVE)) {
        __u8 live = H5I_ST_LIVE;
        bpf_map_update_elem(&H5I_TRACKED, &tid, &live, BPF_ANY);
    }
}

static __always_inline int h5i_do_exec(const char *filename,
                                       const char *const *argv)
{
    if (h5i_admit(H5I_KIND_EXEC) == H5I_ST_NONE) {
        h5i_mark_execed();
        return 0;
    }

    struct h5i_event *e = h5i_begin(H5I_KIND_EXEC);
    if (!e) {
        h5i_mark_execed();
        return 0;
    }
    bpf_probe_read_user_str(e->path, H5I_PATH_LEN, filename);

    if (argv) {
        const char *p = 0;
        /* argv[1] and argv[2], at fixed offsets in `aux`. Those two carry
         * nearly all the signal a single exec has: `-c` plus its script for a
         * shell, the subcommand for a package manager, the target for an
         * interpreter. */
        bpf_probe_read_user(&p, sizeof(p), &argv[1]);
        if (p)
            bpf_probe_read_user_str(e->aux, H5I_AUX_HALF, p);
        p = 0;
        bpf_probe_read_user(&p, sizeof(p), &argv[2]);
        if (p)
            bpf_probe_read_user_str(e->aux + H5I_AUX_HALF, H5I_AUX_HALF, p);

        /* argc, counted without an early exit for the same reason the prefix
         * loop has none: `break` costs the unroll, and the unroll is what
         * keeps `&argv[i]` a constant offset. */
        __s64 argc = 0;
        int ended = 0;
#pragma unroll
        for (int i = 0; i < 32; i++) {
            const char *q = 0;
            if (!ended) {
                if (bpf_probe_read_user(&q, sizeof(q), &argv[i]) != 0 || !q)
                    ended = 1;
                else
                    argc++;
            }
        }
        e->a0 = argc;
    }
    h5i_emit(e);
    /* Past its exec, whatever this task does next is the box's. */
    h5i_mark_execed();
    return 0;
}

SEC("tracepoint/syscalls/sys_enter_execve")
int h5i_sys_enter_execve(struct h5i_sys_enter *ctx)
{
    return h5i_do_exec((const char *)ctx->args[0],
                       (const char *const *)ctx->args[1]);
}

SEC("tracepoint/syscalls/sys_enter_execveat")
int h5i_sys_enter_execveat(struct h5i_sys_enter *ctx)
{
    return h5i_do_exec((const char *)ctx->args[1],
                       (const char *const *)ctx->args[2]);
}

static __always_inline int h5i_do_open(const char *filename, __u64 flags,
                                       __s64 dirfd)
{
    if (h5i_admit(H5I_KIND_OPEN) == H5I_ST_NONE)
        return 0;
    struct h5i_config *cfg = h5i_cfg();
    if (!cfg)
        return 0;

    struct h5i_event *e = h5i_begin(H5I_KIND_OPEN);
    if (!e)
        return 0;
    bpf_probe_read_user_str(e->path, H5I_PATH_LEN, filename);
    e->a0 = (__s64)flags;
    e->a1 = (flags & H5I_O_WRITEISH) ? 1 : 0;
    e->a2 = dirfd;

    /* The volume decision, taken in the kernel. An unfiltered `openat` feed
     * is the single loudest thing a build produces, and shipping it to
     * userspace only to throw 99% of it away is how an observability feature
     * becomes something people switch off (ROADMAP.md D7). */
    if (!cfg->open_all && !e->a1 && !h5i_prefix_hit(e->path) &&
        !(cfg->want_dotenv && h5i_dotenv_hit(e->path))) {
        h5i_drop(e);
        return 0;
    }
    h5i_emit(e);
    return 0;
}

SEC("tracepoint/syscalls/sys_enter_openat")
int h5i_sys_enter_openat(struct h5i_sys_enter *ctx)
{
    return h5i_do_open((const char *)ctx->args[1], ctx->args[2],
                       (__s64)ctx->args[0]);
}

SEC("tracepoint/syscalls/sys_enter_openat2")
int h5i_sys_enter_openat2(struct h5i_sys_enter *ctx)
{
    /* `struct open_how` opens with `__u64 flags`, so the first eight bytes
     * are the flags word. That is UAPI and does not move. */
    __u64 flags = 0;
    bpf_probe_read_user(&flags, sizeof(flags), (const void *)ctx->args[2]);
    return h5i_do_open((const char *)ctx->args[1], flags, (__s64)ctx->args[0]);
}

SEC("tracepoint/syscalls/sys_enter_connect")
int h5i_sys_enter_connect(struct h5i_sys_enter *ctx)
{
    if (h5i_admit(H5I_KIND_CONNECT) == H5I_ST_NONE)
        return 0;

    const void *sa = (const void *)ctx->args[1];
    __u16 family = 0;
    if (bpf_probe_read_user(&family, sizeof(family), sa) != 0)
        return 0;

    struct h5i_event *e = h5i_begin(H5I_KIND_CONNECT);
    if (!e)
        return 0;
    e->a0 = family;
    e->a2 = (__s64)ctx->args[2];

    if (family == H5I_AF_INET) {
        __u16 port_be = 0;
        __u32 addr = 0;
        bpf_probe_read_user(&port_be, sizeof(port_be), (const char *)sa + 2);
        bpf_probe_read_user(&addr, sizeof(addr), (const char *)sa + 4);
        e->a1 = (__s64)((port_be >> 8) | ((port_be & 0xff) << 8));
        __builtin_memcpy(e->aux, &addr, 4);
    } else if (family == H5I_AF_INET6) {
        __u16 port_be = 0;
        bpf_probe_read_user(&port_be, sizeof(port_be), (const char *)sa + 2);
        bpf_probe_read_user(e->aux, 16, (const char *)sa + 8);
        e->a1 = (__s64)((port_be >> 8) | ((port_be & 0xff) << 8));
    } else if (family == H5I_AF_UNIX) {
        /* sun_path is 108 bytes at offset 2. An abstract socket starts with a
         * NUL, so read it as bytes and let the loader render it — a string
         * read would stop at the first byte. */
        bpf_probe_read_user(e->path, 108, (const char *)sa + 2);
        e->a1 = 0;
    }
    h5i_emit(e);
    return 0;
}

SEC("tracepoint/syscalls/sys_enter_socket")
int h5i_sys_enter_socket(struct h5i_sys_enter *ctx)
{
    if (h5i_admit(H5I_KIND_SOCKET) == H5I_ST_NONE)
        return 0;
    struct h5i_event *e = h5i_begin(H5I_KIND_SOCKET);
    if (!e)
        return 0;
    e->a0 = (__s64)ctx->args[0];
    e->a1 = (__s64)ctx->args[1];
    e->a2 = (__s64)ctx->args[2];
    h5i_emit(e);
    return 0;
}

SEC("tracepoint/syscalls/sys_enter_ptrace")
int h5i_sys_enter_ptrace(struct h5i_sys_enter *ctx)
{
    if (h5i_admit(H5I_KIND_PTRACE) == H5I_ST_NONE)
        return 0;
    struct h5i_event *e = h5i_begin(H5I_KIND_PTRACE);
    if (!e)
        return 0;
    e->a0 = (__s64)ctx->args[0];
    e->a1 = (__s64)ctx->args[1];
    h5i_emit(e);
    return 0;
}

SEC("tracepoint/syscalls/sys_enter_bpf")
int h5i_sys_enter_bpf(struct h5i_sys_enter *ctx)
{
    if (h5i_admit(H5I_KIND_BPF) == H5I_ST_NONE)
        return 0;
    struct h5i_event *e = h5i_begin(H5I_KIND_BPF);
    if (!e)
        return 0;
    e->a0 = (__s64)ctx->args[0];
    h5i_emit(e);
    return 0;
}

SEC("tracepoint/syscalls/sys_enter_unshare")
int h5i_sys_enter_unshare(struct h5i_sys_enter *ctx)
{
    if (h5i_admit(H5I_KIND_NSOP) == H5I_ST_NONE)
        return 0;
    struct h5i_event *e = h5i_begin(H5I_KIND_NSOP);
    if (!e)
        return 0;
    e->a0 = (__s64)ctx->args[0];
    e->a1 = 0; /* 0 = unshare, 1 = setns */
    h5i_emit(e);
    return 0;
}

SEC("tracepoint/syscalls/sys_enter_setns")
int h5i_sys_enter_setns(struct h5i_sys_enter *ctx)
{
    if (h5i_admit(H5I_KIND_NSOP) == H5I_ST_NONE)
        return 0;
    struct h5i_event *e = h5i_begin(H5I_KIND_NSOP);
    if (!e)
        return 0;
    e->a0 = (__s64)ctx->args[1];
    e->a1 = 1;
    h5i_emit(e);
    return 0;
}

SEC("tracepoint/syscalls/sys_enter_init_module")
int h5i_sys_enter_init_module(struct h5i_sys_enter *ctx)
{
    if (h5i_admit(H5I_KIND_MODULE) == H5I_ST_NONE)
        return 0;
    struct h5i_event *e = h5i_begin(H5I_KIND_MODULE);
    if (!e)
        return 0;
    e->a0 = 0; /* 0 = init_module, 1 = finit_module */
    bpf_probe_read_user_str(e->path, H5I_PATH_LEN, (const void *)ctx->args[2]);
    h5i_emit(e);
    return 0;
}

SEC("tracepoint/syscalls/sys_enter_finit_module")
int h5i_sys_enter_finit_module(struct h5i_sys_enter *ctx)
{
    if (h5i_admit(H5I_KIND_MODULE) == H5I_ST_NONE)
        return 0;
    struct h5i_event *e = h5i_begin(H5I_KIND_MODULE);
    if (!e)
        return 0;
    e->a0 = 1;
    e->a1 = (__s64)ctx->args[0];
    bpf_probe_read_user_str(e->path, H5I_PATH_LEN, (const void *)ctx->args[1]);
    h5i_emit(e);
    return 0;
}

SEC("tracepoint/syscalls/sys_enter_memfd_create")
int h5i_sys_enter_memfd_create(struct h5i_sys_enter *ctx)
{
    if (h5i_admit(H5I_KIND_MEMFD) == H5I_ST_NONE)
        return 0;
    struct h5i_event *e = h5i_begin(H5I_KIND_MEMFD);
    if (!e)
        return 0;
    bpf_probe_read_user_str(e->path, H5I_PATH_LEN, (const void *)ctx->args[0]);
    e->a0 = (__s64)ctx->args[1];
    h5i_emit(e);
    return 0;
}

SEC("tracepoint/syscalls/sys_enter_mount")
int h5i_sys_enter_mount(struct h5i_sys_enter *ctx)
{
    if (h5i_admit(H5I_KIND_MOUNT) == H5I_ST_NONE)
        return 0;
    struct h5i_event *e = h5i_begin(H5I_KIND_MOUNT);
    if (!e)
        return 0;
    e->a0 = 0; /* 0 = mount, 1 = pivot_root */
    e->a1 = (__s64)ctx->args[3];
    bpf_probe_read_user_str(e->path, H5I_PATH_LEN, (const void *)ctx->args[1]);
    bpf_probe_read_user_str(e->aux, H5I_AUX_HALF, (const void *)ctx->args[0]);
    h5i_emit(e);
    return 0;
}

SEC("tracepoint/syscalls/sys_enter_pivot_root")
int h5i_sys_enter_pivot_root(struct h5i_sys_enter *ctx)
{
    if (h5i_admit(H5I_KIND_MOUNT) == H5I_ST_NONE)
        return 0;
    struct h5i_event *e = h5i_begin(H5I_KIND_MOUNT);
    if (!e)
        return 0;
    e->a0 = 1;
    bpf_probe_read_user_str(e->path, H5I_PATH_LEN, (const void *)ctx->args[0]);
    bpf_probe_read_user_str(e->aux, H5I_AUX_HALF, (const void *)ctx->args[1]);
    h5i_emit(e);
    return 0;
}

/* ------------------------------------------------------- the process tree */

SEC("tracepoint/sched/sched_process_fork")
int h5i_sched_process_fork(struct h5i_sched_fork *ctx)
{
    /* Resolve the parent first: a PENDING parent that is about to fork must
     * be classified before its child inherits from it. */
    __u8 parent = h5i_state();
    if (parent == H5I_ST_NONE)
        return 0;

    __u32 child = (__u32)ctx->child_pid;
    __u8 next;
    if (parent == H5I_ST_SELF) {
        /* h5i forked something. Whether it is the payload (a new process) or
         * one of h5i's own threads is not knowable here; the child's first
         * event settles it. */
        next = H5I_ST_PEND;
    } else {
        /* Already the box's. A child inherits the parent's exec state, so a
         * fork-only worker — Python multiprocessing, a shell subshell — is
         * not silently muted for never having execed. */
        next = parent;
    }
    bpf_map_update_elem(&H5I_TRACKED, &child, &next, BPF_ANY);

    if (parent != H5I_ST_SELF && h5i_kind_on(H5I_KIND_FORK)) {
        struct h5i_event *e = h5i_begin(H5I_KIND_FORK);
        if (e) {
            e->a0 = (__s64)ctx->child_pid;
            e->a1 = (__s64)ctx->parent_pid;
            h5i_emit(e);
        }
    }
    return 0;
}

SEC("tracepoint/sched/sched_process_exit")
int h5i_sched_process_exit(struct h5i_sched_exit *ctx)
{
    __u32 tid = (__u32)ctx->pid;
    __u8 *st = bpf_map_lookup_elem(&H5I_TRACKED, &tid);
    if (!st)
        return 0;
    __u8 v = *st;
    if ((v == H5I_ST_PRE || v == H5I_ST_LIVE) && h5i_kind_on(H5I_KIND_EXIT)) {
        struct h5i_event *e = h5i_begin(H5I_KIND_EXIT);
        if (e) {
            e->a0 = (__s64)ctx->pid;
            h5i_emit(e);
        }
    }
    /* Pruned here rather than left to expire. The pid-reuse window this
     * leaves open is one scheduler quantum wide and is stated in the limits
     * (ROADMAP.md D13.5) rather than papered over with a generation counter
     * that would cost more than the exposure. */
    bpf_map_delete_elem(&H5I_TRACKED, &tid);
    return 0;
}
