// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::cell::Cell;

use egcl_compiler::reader::{self, ReaderMacroKind, ReaderStream};
use egcl_rt::error::EgclError;
use egcl_rt::value::{EgclVal, NIL, TAG_MASK};
use egcl_rt::{Collector, HeapCollector};

thread_local! {
    // Reader tables are pinned; the test's intrusive root retains the table.
    // This cell supplies the current readtable to recursive reader operations.
    static TABLE: Cell<EgclVal> = const { Cell::new(NIL) };
}

fn current_table() -> EgclVal {
    TABLE.with(Cell::get)
}

struct ResetHook;
impl Drop for ResetHook {
    fn drop(&mut self) {
        reader::set_readtable_getter(None);
        TABLE.with(|table| table.set(NIL));
    }
}

struct Input {
    chars: Vec<char>,
    position: usize,
    moved: bool,
}
impl Input {
    fn new(source: &str) -> Self {
        Self {
            chars: source.chars().collect(),
            position: 0,
            moved: false,
        }
    }
}
impl ReaderStream for Input {
    fn read_char(&mut self) -> Result<Option<char>, EgclError> {
        let ch = self.chars.get(self.position).copied();
        self.position += usize::from(ch.is_some());
        Ok(ch)
    }

    fn unread_char(&mut self, ch: char) -> Result<(), EgclError> {
        self.position -= 1;
        assert_eq!(self.chars[self.position], ch);
        Ok(())
    }

    fn invoke_macro(
        &mut self,
        _: EgclVal,
        ch: char,
        kind: ReaderMacroKind,
    ) -> Result<Vec<EgclVal>, EgclError> {
        match kind {
            ReaderMacroKind::Dispatch(Some(label)) => {
                assert!(matches!(ch, '=' | '#'));
                reader::read_standard_label_from_stream(self, ch, label as u64, 10, false)
            }
            ReaderMacroKind::Plain => {
                assert_eq!(ch, '!');
                // No Rust EgclVal root retains this snapshot. The shared label
                // context (and the outer label operation) must update its copy.
                let before = reader::read_standard_label_from_stream(
                    &mut Input::new(""),
                    '#',
                    1,
                    10,
                    false,
                )?[0]
                    .to_raw();
                HeapCollector::new().minor_gc().expect("callback minor GC");
                let after = reader::read_standard_label_from_stream(
                    &mut Input::new(""),
                    '#',
                    1,
                    10,
                    false,
                )?[0];
                assert_ne!(before, after.to_raw(), "the in-progress label really moved");
                self.moved = true;
                Ok(vec![after])
            }
            _ => panic!("unexpected reader macro invocation"),
        }
    }
}

#[test]
fn callable_labels_preserve_moving_placeholders_through_custom_callbacks() {
    egcl_rt::gc::ensure_heap_initialized();
    let _thread = egcl_rt::current_thread();
    egcl_rt::rooted!(table = reader::make_readtable(None).unwrap());
    let handler = EgclVal::from_symbol_index(reader::intern_symbol("LABEL-GC-HANDLER"));
    reader::set_macro_character(*table, '!', handler, false).unwrap();
    reader::set_dispatch_macro_character(*table, '#', '=', handler).unwrap();
    reader::set_dispatch_macro_character(*table, '#', '#', handler).unwrap();
    TABLE.with(|value| value.set(*table));
    reader::set_readtable_getter(Some(current_table));
    let _reset = ResetHook;
    // Drain setup objects before creating the placeholder whose movement is
    // required below. All subsequent stream operations are Rust-only except
    // the explicit collection in the custom reader callback.
    HeapCollector::new().minor_gc().unwrap();

    let mut input = Input::new("#1=(!)");
    egcl_rt::rooted!(value = reader::read_from_stream(&mut input, 10, false, false).unwrap());
    assert!(input.moved, "the custom reader callback must execute");
    assert!(value.is_cons());
    let (car, cdr) = unsafe {
        let ptr = (value.to_raw() & !TAG_MASK) as *const EgclVal;
        (*ptr, *ptr.add(1))
    };
    assert_eq!(
        car, *value,
        "the relocated placeholder becomes the self-reference"
    );
    assert_eq!(cdr, NIL);
    assert!(
        reader::read_standard_label_from_stream(&mut Input::new(""), '#', 1, 10, false,).is_err(),
        "the completed outer READ must release its label scope"
    );
}
