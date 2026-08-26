//! Wasm ABI: the pinned six-symbol boundary over the sans-io core. Compiled
//! only for `wasm32` (`#[cfg(target_arch = "wasm32")]` in `lib.rs`), so the
//! native build and `cargo test` never see the allocator / panic handler.
//!
//! Exports (no imports at all — loadable by plain `WebAssembly.instantiate` in
//! a browser/Node AND by any WASI runtime):
//!   memory
//!   alloc(len: i32) -> i32          host obtains a guest buffer to write into
//!   dealloc(ptr: i32, len: i32)     no-op under the bump allocator; kept so
//!                                   the ABI outlives the allocator choice
//!   agent_init(ptr, len) -> u64     init JSON in; first effect JSON out
//!   agent_step(ptr, len) -> u64     event JSON in; next effect JSON out
//!   agent_dump() -> u64             deterministic transcript JSON out
//!
//! Return convention: (ptr << 32) | len of guest-owned UTF-8 JSON, valid until
//! the NEXT exported call — and `alloc()` IS an export call, so the host must
//! copy a returned effect out BEFORE calling `alloc` for the next event. (The
//! current bump allocator would make the lazy order accidentally safe; the
//! rule is stated strictly so a future real allocator cannot break hosts.)
//!
//! Build with `scripts/build-wasm.sh` (needs `rustup target add
//! wasm32-unknown-unknown`); the target's prebuilt core/alloc mean no
//! `-Zbuild-std`, no nightly, and no external crates.

#![allow(static_mut_refs)]

use crate::agent::Agent;
use crate::proto;
use alloc::string::String;
use core::alloc::{GlobalAlloc, Layout};

// ---- allocator: bump over memory.grow, free is a no-op ----

struct Bump;

static mut NEXT: usize = 0;

unsafe impl GlobalAlloc for Bump {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        unsafe {
            if NEXT == 0 {
                // Start allocations one page past current memory end.
                NEXT = core::arch::wasm32::memory_size(0) * 65536;
            }
            let align = layout.align().max(8);
            let start = (NEXT + align - 1) & !(align - 1);
            let end = start + layout.size();
            let need_pages = (end + 65535) / 65536;
            let have_pages = core::arch::wasm32::memory_size(0);
            if need_pages > have_pages
                && core::arch::wasm32::memory_grow(0, need_pages - have_pages) == usize::MAX
            {
                return core::ptr::null_mut();
            }
            NEXT = end;
            start as *mut u8
        }
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {}
}

#[global_allocator]
static ALLOCATOR: Bump = Bump;

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    core::arch::wasm32::unreachable()
}

// ---- session state: one agent per module instance ----

static mut AGENT: Option<Agent> = None;
static mut OUT: Option<String> = None;

/// Park `s` in a static and return (ptr << 32) | len. The buffer lives until
/// the next export call replaces it.
fn ret(s: String) -> u64 {
    unsafe {
        OUT = Some(s);
        let out = OUT.as_ref().unwrap();
        ((out.as_ptr() as u64) << 32) | out.len() as u64
    }
}

unsafe fn take_input(ptr: *mut u8, len: usize) -> Result<String, ()> {
    unsafe {
        let slice = core::slice::from_raw_parts(ptr, len);
        match core::str::from_utf8(slice) {
            Ok(s) => Ok(String::from(s)),
            Err(_) => Err(()),
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn alloc(len: i32) -> i32 {
    let layout = Layout::from_size_align(len.max(1) as usize, 1).unwrap();
    unsafe { ALLOCATOR.alloc(layout) as i32 }
}

#[unsafe(no_mangle)]
pub extern "C" fn dealloc(_ptr: i32, _len: i32) {}

#[unsafe(no_mangle)]
pub extern "C" fn agent_init(ptr: i32, len: i32) -> u64 {
    let input = match unsafe { take_input(ptr as *mut u8, len as usize) } {
        Ok(s) => s,
        Err(()) => return ret(proto::fatal_json("init input is not UTF-8")),
    };
    match proto::init_from_json(&input) {
        Ok((agent, first_effect)) => {
            unsafe { AGENT = Some(agent) };
            ret(first_effect)
        }
        Err(e) => ret(proto::fatal_json(&e)),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn agent_step(ptr: i32, len: i32) -> u64 {
    let input = match unsafe { take_input(ptr as *mut u8, len as usize) } {
        Ok(s) => s,
        Err(()) => return ret(proto::fatal_json("step input is not UTF-8")),
    };
    match unsafe { AGENT.as_mut() } {
        Some(agent) => ret(proto::step_json(agent, &input)),
        None => ret(proto::fatal_json("agent_step before agent_init")),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn agent_dump() -> u64 {
    match unsafe { AGENT.as_ref() } {
        Some(agent) => ret(proto::dump_json(agent)),
        None => ret(proto::fatal_json("agent_dump before agent_init")),
    }
}
