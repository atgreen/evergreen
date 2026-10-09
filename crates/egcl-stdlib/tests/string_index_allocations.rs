// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use egcl_rt::value::EgclVal;
use egcl_stdlib::sequences;

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

#[test]
fn simple_string_reads_do_not_allocate() {
    // Reader strings exercise both compact BASE-CHAR and wide CHARACTER
    // storage, including a wide string with an extended large-object header.
    for (text, last) in [
        ("café".to_owned(), 'é'),
        ("a界🦀".to_owned(), '🦀'),
        (format!("{}界", "a".repeat(131_072)), '界'),
    ] {
        for registered in [false, true] {
            let source = format!("\"{text}\"");
            egcl_rt::rooted!(string = egcl_compiler::reader::read_from_string(&source).unwrap().0);
            if registered {
                egcl_stdlib::pathnames::register_string(*string, &text);
            }
            let n = text.chars().count();
            type StringRead = fn(EgclVal, usize) -> usize;
            let operations: [(&str, StringRead, usize); 4] = [
                (
                    "ELT",
                    |s, i| sequences::elt(s, i).unwrap().as_char() as usize,
                    last as usize,
                ),
                (
                    "character access",
                    |s, i| sequences::string_char_at(s, i).unwrap() as usize,
                    last as usize,
                ),
                ("LENGTH", |s, _| sequences::length(s).unwrap(), n),
                (
                    "character count",
                    |s, _| sequences::string_char_count(s).unwrap(),
                    n,
                ),
            ];
            let mut allocations = Vec::new();
            for (name, operation, expected) in operations {
                // Exclude lazy runtime setup from the measured reads.
                assert_eq!(operation(*string, n - 1), expected);
                BYTES.with(|bytes| bytes.set(0));
                COUNT.with(|count| count.set(true));
                let result = std::hint::black_box(operation)(*string, n - 1);
                COUNT.with(|count| count.set(false));
                assert_eq!(result, expected);
                allocations.push((name, BYTES.with(Cell::get)));
            }
            assert!(
                allocations.iter().all(|(_, bytes)| *bytes == 0),
                "simple string length {n}, registered={registered}: {allocations:?}"
            );
            assert_eq!(sequences::string_char_at(*string, n), None);
            assert!(sequences::elt(*string, n).is_err());
        }
    }
}

#[test]
fn a_registered_pathname_is_not_a_string_sequence() {
    egcl_rt::rooted!(source = egcl_stdlib::make_lisp_string("/tmp/string-read-test.lisp"));
    egcl_rt::rooted!(
        pathname = egcl_stdlib::parse_namestring(*source, None, None)
            .unwrap()
            .0
    );
    egcl_stdlib::pathnames::register_string(*pathname, "/tmp/string-read-test.lisp");
    assert_eq!(sequences::string_char_at(*pathname, 0), None);
    assert_eq!(sequences::string_char_count(*pathname), None);
    assert!(sequences::length(*pathname).is_err());
    assert!(sequences::elt(*pathname, 0).is_err());
}
