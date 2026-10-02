// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use egcl_compiler::reader::read_from_string;

#[test]
fn substitute_character_name_is_case_insensitive() {
    for source in [r"#\Sub", r"#\sub", r"#\SUB", r"#\sUb"] {
        let (value, _) = read_from_string(source).expect("read ASCII substitute name");
        assert!(value.is_character());
        assert_eq!(value.as_char(), '\u{1a}');
    }
}
