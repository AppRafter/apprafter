// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! S6 (D.4a T58): neither secret-bearing frame leaves its secret in freed heap memory. This
//! binary's global allocator scans every block it frees for the test key and the test item: the
//! codec must write the key straight from its own buffer and read it straight into the caller's,
//! and read an item into an exact-size buffer that is zeroed before it is freed. A whole-body
//! `Vec` anywhere on the way fails the test (mutants 1–3 of Task 2).

use std::alloc::{GlobalAlloc, Layout, System};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use apprafter::holder::protocol::{
    read_init, read_reply, write_init, write_secret, InitHeader, Reply, StoreBinding,
};
use apprafter_core::vault::{DataKey, VaultId};

/// Patterns no allocation of this test holds by chance.
const KEY: [u8; 32] = *b"S6-test-key:never-in-freed-heap!";
const ITEM: &[u8] = b"S6-test-item:a-token-a-holder-sends-to-a-served-client";

static ARMED: AtomicBool = AtomicBool::new(false);
static LEAKS: AtomicUsize = AtomicUsize::new(0);

fn holds(block: &[u8], needle: &[u8]) -> bool {
    block.len() >= needle.len() && block.windows(needle.len()).any(|w| w == needle)
}

struct Scanning;

// SAFETY: every call is forwarded to the system allocator; `dealloc` only reads the block
// it is about to free, within its layout.
unsafe impl GlobalAlloc for Scanning {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded unchanged.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if ARMED.load(Ordering::Relaxed) {
            // SAFETY: `ptr` is a live block of `layout.size()` bytes this allocator handed out.
            let block = unsafe { std::slice::from_raw_parts(ptr, layout.size()) };
            if holds(block, &KEY) || holds(block, &ITEM[..32]) {
                LEAKS.fetch_add(1, Ordering::Relaxed);
            }
        }
        // SAFETY: forwarded unchanged.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Scanning = Scanning;

/// A writer into a buffer on the stack: what it is given is copied nowhere on the heap.
struct Stack<'a> {
    buf: &'a mut [u8],
    at: usize,
}

impl Write for Stack<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let n = bytes.len().min(self.buf.len() - self.at);
        self.buf[self.at..self.at + n].copy_from_slice(&bytes[..n]);
        self.at += n;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn neither_secret_frame_leaves_its_secret_in_freed_memory() {
    let init = InitHeader {
        store: StoreBinding {
            id: "0123456789abcdef".into(),
            root: PathBuf::from("/s"),
        },
        base: PathBuf::from("/r"),
        vault_id: VaultId([1; 16]),
        minutes: 5,
        window_secs: None,
        poll_leader: false,
        client_pid: 1,
    };
    let key = DataKey::from_bytes(&KEY);
    let mut frame = [0u8; 512];
    let mut item_frame = [0u8; 256];
    let mut got = [0u8; 32];
    ARMED.store(true, Ordering::SeqCst);
    let n = {
        let mut w = Stack {
            buf: &mut frame,
            at: 0,
        };
        write_init(&mut w, &init, &key).unwrap();
        w.at
    };
    let back = read_init(&mut &frame[..n], &mut got).unwrap();
    let m = {
        let mut w = Stack {
            buf: &mut item_frame,
            at: 0,
        };
        write_secret(&mut w, ITEM).unwrap();
        w.at
    };
    let reply = read_reply(&mut &item_frame[..m]).unwrap();
    let item_ok = matches!(&reply, Reply::Item(v) if v.as_slice() == ITEM);
    drop(reply);
    drop(back);
    drop(key);
    ARMED.store(false, Ordering::SeqCst);
    let key_ok = got == KEY;
    frame.fill(0);
    item_frame.fill(0);
    got.fill(0);
    assert!(key_ok && item_ok, "the codec must still carry both secrets");
    assert_eq!(
        LEAKS.load(Ordering::SeqCst),
        0,
        "a freed heap block held the key or the item: the codec copied it through a plain buffer (S6)"
    );
}
