// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use egcl_rt::jit::JitBuffer;

#[test]
fn installed_code_owns_the_dwarf_image_and_range() {
    let code = [0; 16]; // Metadata-only test; never execute these bytes.
    let mut buffer = JitBuffer::new(&code).unwrap();
    let weak = buffer
        .install_debug_info("COMMON-LISP-USER::INSTALLED")
        .unwrap();
    let image = weak.upgrade().unwrap();
    let start = buffer.as_ptr() as u64;
    assert_eq!(
        image.function_name(start).unwrap().as_deref(),
        Some("COMMON-LISP-USER::INSTALLED")
    );
    assert_eq!(
        image.function_name(start + 15).unwrap().as_deref(),
        Some("COMMON-LISP-USER::INSTALLED")
    );
    assert_eq!(image.function_name(start - 1).unwrap(), None);
    assert_eq!(image.function_name(start + 16).unwrap(), None);
    assert_eq!(&image.elf_bytes()[..4], b"\x7fELF");
    drop(image);
    drop(buffer);
    assert!(
        weak.upgrade().is_none(),
        "debug image must retire with executable code"
    );
}

#[test]
fn logical_native_frames_get_their_name_from_dwarf() {
    let mut buffer = JitBuffer::new(&[0; 16]).unwrap();
    let weak = buffer
        .install_debug_info("COMMON-LISP-USER::DWARF-FRAME")
        .unwrap();
    let info = egcl_rt::CodeInfo::new_jit(&[], &[], weak.clone());
    let stack = egcl_rt::current_stack();
    stack.push_frame(egcl_rt::value::NIL, info, 0, 0).unwrap();
    let snapshot = egcl_rt::debug_stack::capture_current(1);
    stack.pop_frame();
    assert_eq!(
        snapshot[0].function.as_deref(),
        Some("COMMON-LISP-USER::DWARF-FRAME")
    );
    drop(buffer);
    assert!(weak.upgrade().is_none());
    assert!(
        info.function_name().is_none(),
        "retired metadata must not alias reused code"
    );
    assert_eq!(
        snapshot[0].function.as_deref(),
        Some("COMMON-LISP-USER::DWARF-FRAME")
    );
}
