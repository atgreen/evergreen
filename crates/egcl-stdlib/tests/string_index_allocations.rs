// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

struct CountingAllocator;
thread_local! {
    static COUNT: Cell<bool> = const { Cell::new(false) };
    static BYTES: Cell<usize> = const { Cell::new(0) };
}

fn record(size: usize) {
    if COUNT.try_with(Cell::get).unwrap_or(false) {
        BYTES.with(|bytes| bytes.set(bytes.get() + size));
    }
}

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record(size);
        unsafe { System.realloc(ptr, layout, size) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

#[test]
fn elt_does_not_copy_a_large_string_to_read_one_character() {
    let text = format!("{}界", "a".repeat(65_536));
    let string = egcl_stdlib::make_lisp_string_fresh(&text);
    egcl_rt::rooted!(string = string);
    // Warm one-time runtime state before measuring the sequence operation.
    assert_eq!(egcl_stdlib::elt(*string, 0).unwrap().as_char(), 'a');
    BYTES.with(|bytes| bytes.set(0));
    COUNT.with(|count| count.set(true));
    let result = egcl_stdlib::elt(*string, 65_536);
    COUNT.with(|count| count.set(false));
    assert_eq!(result.unwrap().as_char(), '界');
    let bytes = BYTES.with(Cell::get);
    assert!(
        bytes < 1024,
        "one ELT allocated {bytes} bytes for a 64K string"
    );
}
