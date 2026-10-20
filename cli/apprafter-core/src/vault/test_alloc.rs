// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The allocator of apprafter-core's unit-test binary (this module is `cfg(test)`): the
//! system's, watched for two facts the KDF tests assert, per thread so parallel tests do not
//! disturb each other —
//!
//! - whether a freed Argon2 working-memory buffer still held data: an `argon2::Block` is
//!   1 KiB and 64-byte aligned, and no other allocation of these tests is both;
//! - the largest allocation asked for, refused ones included: "refused before allocating".

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

struct Watching;

#[global_allocator]
static ALLOCATOR: Watching = Watching;

thread_local! {
    static UNZEROED_BLOCK_FREES: Cell<usize> = const { Cell::new(0) };
    static LARGEST: Cell<usize> = const { Cell::new(0) };
}

fn note_size(size: usize) {
    let _ = LARGEST.try_with(|largest| {
        if size > largest.get() {
            largest.set(size);
        }
    });
}

fn is_argon2_memory(layout: Layout) -> bool {
    layout.align() == 64 && layout.size() >= 8 * 1024 && layout.size().is_multiple_of(1024)
}

// SAFETY: every method forwards to `System` with the caller's own arguments; the only extra
// work reads a live allocation (in `dealloc`, before it is handed back) and two `Cell`s of a
// const-initialised thread local without a destructor, which never allocates.
unsafe impl GlobalAlloc for Watching {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note_size(layout.size());
        // SAFETY: the caller's contract for `alloc`, passed on unchanged.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        note_size(layout.size());
        // SAFETY: as for `alloc`.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note_size(new_size);
        // SAFETY: as for `alloc`.
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if is_argon2_memory(layout) {
            // SAFETY: `ptr` is a live allocation of `layout.size()` bytes until the
            // `System.dealloc` below, and initialised: the only buffers of this shape in these
            // tests are argon2's (`alloc_zeroed`, `argon2-0.6.0/src/block.rs:168-178`) and the
            // vault's (`resize`d or `vec!`-filled before use, and wiped by `Zeroizing`).
            let bytes = unsafe { std::slice::from_raw_parts(ptr, layout.size()) };
            if bytes.iter().any(|&b| b != 0) {
                let _ = UNZEROED_BLOCK_FREES.try_with(|n| n.set(n.get() + 1));
            }
        }
        // SAFETY: as for `alloc`.
        unsafe { System.dealloc(ptr, layout) }
    }
}

/// How many Argon2 working-memory buffers this thread freed while they still held data.
pub(crate) fn unzeroed_block_frees() -> usize {
    UNZEROED_BLOCK_FREES.with(Cell::get)
}

/// Forget this thread's largest allocation so far.
pub(crate) fn reset_largest() {
    LARGEST.with(|largest| largest.set(0));
}

/// This thread's largest allocation (asked for, granted or not) since [`reset_largest`].
pub(crate) fn largest() -> usize {
    LARGEST.with(Cell::get)
}
