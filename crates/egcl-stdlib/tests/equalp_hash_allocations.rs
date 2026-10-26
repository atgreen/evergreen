// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use egcl_stdlib::hashtable::{HashTest, MakeHashTableOptions, gethash, make_hash_table};

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
fn equalp_numeric_hashing_does_not_allocate() {
    egcl_rt::rooted!(
        table = make_hash_table(&MakeHashTableOptions {
            test: HashTest::Equalp,
            ..Default::default()
        })
        .unwrap()
    );
    // An empty table isolates key hashing from numeric comparison, which still
    // uses temporary Rust big integers. Setup and lazy initialization are not
    // included in the allocator measurement.
    for source in [
        "0",
        "-1",
        "9007199254740993",
        "1152921504606846976",
        "18446744073709551616",
        "-18446744073709551617",
        "340282366920938463463374607431768211456",
        "1/2",
        "-3/8",
        "1/3",
        "9007199254740993/2",
        "1/18446744073709551616",
        "1/340282366920938463463374607431768211456",
        "340282366920938463463374607431768211457/3",
        "0.5d0",
        "-0d0",
        "#c(1d0 0d0)",
    ] {
        egcl_rt::rooted!(key = egcl_compiler::reader::read_from_string(source).unwrap().0);
        assert!(!gethash(*key, *table, egcl_rt::value::NIL).unwrap().1);
        BYTES.with(|bytes| bytes.set(0));
        COUNT.with(|count| count.set(true));
        let result = gethash(std::hint::black_box(*key), *table, egcl_rt::value::NIL);
        COUNT.with(|count| count.set(false));
        assert!(!result.unwrap().1);
        assert_eq!(
            BYTES.with(Cell::get),
            0,
            "hashing {source} allocated Rust memory"
        );
    }
}
