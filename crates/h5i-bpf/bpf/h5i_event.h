/* SPDX-License-Identifier: (BSD-2-Clause OR GPL-2.0)
 *
 * h5i_event.h — the wire contract between the probe and the loader.
 *
 * This file is the single definition of what crosses the ring buffer. The
 * Rust side (`src/event.rs`) mirrors it field for field and asserts the size
 * at compile time; every event also carries a magic word and a version, so a
 * probe object and a loader that disagree are caught at the first record
 * rather than silently misparsed into plausible-looking nonsense.
 *
 * Fixed size, deliberately. A variable-length record would save bandwidth we
 * do not need and would cost a length field the verifier has to be convinced
 * about on every path.
 */

#ifndef H5I_EVENT_H
#define H5I_EVENT_H

#define H5I_EVENT_MAGIC 0x68356945u /* "h5iE" */
#define H5I_EVENT_VERSION 1u

#define H5I_COMM_LEN 16
#define H5I_PATH_LEN 256
#define H5I_AUX_LEN 192
/* `aux` carries two independent strings at fixed offsets rather than one
 * packed pair: a dynamic write offset is exactly the kind of thing that turns
 * a trivially verifiable program into an argument with the verifier. */
#define H5I_AUX_HALF 96

/* Event kinds. Numbers are wire values: append, never renumber. */
#define H5I_KIND_EXEC 1
#define H5I_KIND_OPEN 2
#define H5I_KIND_CONNECT 3
#define H5I_KIND_SOCKET 4
#define H5I_KIND_PTRACE 5
#define H5I_KIND_BPF 6
#define H5I_KIND_NSOP 7
#define H5I_KIND_MODULE 8
#define H5I_KIND_MEMFD 9
#define H5I_KIND_MOUNT 10
#define H5I_KIND_FORK 11
#define H5I_KIND_EXIT 12

/* Statistics slots in the per-CPU counter array. */
#define H5I_STAT_EMITTED 0
#define H5I_STAT_LOST 1
#define H5I_STAT_FILTERED 2
#define H5I_STAT_MAX 3

/* Prefix filter geometry. Both are unrolled loop bounds in the probe, so they
 * are the reason `openat` filtering costs a fixed, constant-index comparison
 * rather than a data-dependent loop. */
#define H5I_MAX_PREFIX 16
#define H5I_PREFIX_LEN 64

struct h5i_event {
    __u32 magic;
    __u16 version;
    __u16 kind;
    __u64 ts_ns;
    __u32 tgid;
    __u32 tid;
    /* Always zero on the wire: the probe reads no kernel structure, so it has
     * no parent pointer (ROADMAP.md D5). The loader fills this in from the
     * Fork events it has already seen, which is the same answer without the
     * CO-RE dependency. */
    __u32 ppid;
    __u32 uid;
    /* Kind-specific scalars; see `Event::decode` on the Rust side for the
     * per-kind meaning, which is documented in exactly one place. */
    __s64 a0;
    __s64 a1;
    __s64 a2;
    char comm[H5I_COMM_LEN];
    char path[H5I_PATH_LEN];
    char aux[H5I_AUX_LEN];
};

/* Per-run knobs, one element of an ARRAY map, written by the loader after
 * load and before attach. */
struct h5i_config {
    /* Bit i set ⇒ kind i is emitted. Kind numbers are small, so a u64 covers
     * the catalogue with room to grow. */
    __u64 kind_mask;
    /* How many entries of the prefix array are live. */
    __u32 prefix_count;
    /* Emit `Open` for read-only opens that hit no prefix. Off by default:
     * this is the knob that decides whether a `cargo build` ships a hundred
     * thousand events or a hundred. */
    __u32 open_all;
    /* Also emit any open whose path contains `/.env`.
     *
     * A separate flag because it is the one filter that needs *substring*
     * semantics, and the prefix map cannot express those: a `.env` sits at the
     * end of a path whose beginning is a directory nobody enumerated. It is a
     * scan rather than a map lookup, so it is switched off with the rule that
     * wants it rather than paid for unconditionally. */
    __u32 want_dotenv;
    /* Padding to keep the struct's size explicit on both sides of the wire. */
    __u32 _pad;
};

struct h5i_prefix {
    __u32 len;
    char s[H5I_PREFIX_LEN];
};

/* Per-pid scope state. The values are a state machine, not flags:
 *
 *   SELF  h5i's own thread. Never emitted; its forks become PENDING.
 *   PEND  forked from something in the set, not yet classified. The first
 *         event the task raises resolves it: a task whose tid equals its tgid
 *         is a new process (the payload, or something the payload spawned) and
 *         becomes PRE; anything else is a thread of h5i and becomes SELF.
 *   PRE   in the box's tree but has not execed yet. This is the window in
 *         which h5i's own `pre_exec` code runs — applying Landlock, opening
 *         the ruleset paths — and attributing that to the box would be
 *         reporting h5i's confinement work as the box's behaviour. Only Exec
 *         (and the tree bookkeeping) is emitted here.
 *   LIVE  in the box's tree, past its exec. Everything is emitted.
 */
#define H5I_ST_SELF 0
#define H5I_ST_PEND 1
#define H5I_ST_PRE 2
#define H5I_ST_LIVE 3
/* Not a stored value: what a lookup miss returns. */
#define H5I_ST_NONE 255

#endif /* H5I_EVENT_H */
