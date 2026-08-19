/* SPDX-License-Identifier: (BSD-2-Clause OR GPL-2.0)
 *
 * h5i_bpf.h — the whole BPF-side runtime, vendored.
 *
 * There is deliberately no `#include` of anything in this file. Not of
 * <linux/bpf.h> (its include chain reaches asm/types.h, which is absent on a
 * host with no arch headers installed — this one, for instance), and not of
 * libbpf's bpf_helpers.h (libbpf is not a build dependency and adding one
 * would undo the reason aya was chosen; ROADMAP.md D4).
 *
 * What that costs is the twenty-odd constants and prototypes below, all of
 * them stable kernel ABI. What it buys is that the probe compiles with
 * nothing but `clang -target bpf`, which is the difference between a feature
 * every contributor can build and one that needs a package install first.
 *
 * Everything here is UAPI-stable. Nothing here describes a kernel *structure*
 * — that is the CO-RE cut (ROADMAP.md D5), and it is what lets one object
 * load on every kernel from 5.8 up without BTF, without vmlinux.h and
 * without a relocating loader.
 */

#ifndef H5I_BPF_H
#define H5I_BPF_H

typedef unsigned char __u8;
typedef signed char __s8;
typedef unsigned short __u16;
typedef short __s16;
typedef unsigned int __u32;
typedef int __s32;
typedef unsigned long long __u64;
typedef long long __s64;

#define SEC(NAME) __attribute__((section(NAME), used))
#define __always_inline inline __attribute__((always_inline))

/* BTF map definitions expand to pointer-to-array types whose array length
 * carries the value. This is libbpf's encoding and aya parses the same one;
 * spelling the macros here rather than including them changes nothing about
 * the bytes clang emits. */
#define __uint(name, val) int (*name)[val]
#define __type(name, val) typeof(val) *name
#define __array(name, val) typeof(val) *name[]

/* enum bpf_map_type — the four this probe uses. */
#define BPF_MAP_TYPE_HASH 1
#define BPF_MAP_TYPE_ARRAY 2
#define BPF_MAP_TYPE_PERCPU_ARRAY 6
#define BPF_MAP_TYPE_RINGBUF 27

/* map update flags */
#define BPF_ANY 0
#define BPF_NOEXIST 1
#define BPF_EXIST 2

/* Helper ids, in __BPF_FUNC_MAPPER order. Every one of these is stable: a
 * helper's id is part of the kernel's ABI and is never reused. */
static void *(*bpf_map_lookup_elem)(void *map, const void *key) = (void *)1;
static long (*bpf_map_update_elem)(void *map, const void *key, const void *value,
                                   __u64 flags) = (void *)2;
static long (*bpf_map_delete_elem)(void *map, const void *key) = (void *)3;
static __u64 (*bpf_ktime_get_ns)(void) = (void *)5;
static __u64 (*bpf_get_current_pid_tgid)(void) = (void *)14;
static __u64 (*bpf_get_current_uid_gid)(void) = (void *)15;
static long (*bpf_get_current_comm)(void *buf, __u32 size_of_buf) = (void *)16;
static long (*bpf_probe_read_user)(void *dst, __u32 size,
                                   const void *unsafe_ptr) = (void *)112;
static long (*bpf_probe_read_user_str)(void *dst, __u32 size,
                                       const void *unsafe_ptr) = (void *)114;
static void *(*bpf_ringbuf_reserve)(void *ringbuf, __u64 size, __u64 flags) = (void *)131;
static void (*bpf_ringbuf_submit)(void *data, __u64 flags) = (void *)132;
static void (*bpf_ringbuf_discard)(void *data, __u64 flags) = (void *)133;

/* The syscall-entry tracepoint context.
 *
 * This is the one layout the probe depends on, and it is fixed ABI: the four
 * `common_*` fields occupy the first eight bytes on every architecture, the
 * syscall number follows in the next eight, and the six arguments are
 * register-width from there. The loader re-checks it against
 * /sys/kernel/tracing/events/.../format when that file is readable and
 * refuses to attach if a kernel ever moved a field, so a wrong assumption
 * here fails loudly instead of misreading arguments (ROADMAP.md D5). */
struct h5i_sys_enter {
    __u64 common;
    __s64 syscall_nr;
    __u64 args[6];
};

/* sched:sched_process_fork. Field offsets from the tracepoint's `format`:
 * parent_comm[16] at 8, parent_pid at 24, child_comm[16] at 28,
 * child_pid at 44. Checked by the loader, same as above. */
struct h5i_sched_fork {
    __u64 common;
    char parent_comm[16];
    __s32 parent_pid;
    char child_comm[16];
    __s32 child_pid;
};

/* sched:sched_process_exit. comm[16] at 8, pid at 24. */
struct h5i_sched_exit {
    __u64 common;
    char comm[16];
    __s32 pid;
};

#endif /* H5I_BPF_H */
