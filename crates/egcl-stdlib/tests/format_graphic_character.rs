// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use egcl_rt::EgclVal;
use egcl_stdlib::format::{LayoutOptions, format_to_layout, graphic_char_p, render_layout};

fn rendered(control: &str, character: char) -> String {
    let doc = format_to_layout(control, &[EgclVal::from_char(character)]).unwrap();
    render_layout(&doc, &LayoutOptions::default())
}

#[test]
fn colon_character_output_agrees_with_graphic_character_classification() {
    for code in 0..0x10000 {
        let Some(character) = char::from_u32(code) else {
            continue;
        };
        if character != ' ' && graphic_char_p(character) {
            assert_eq!(
                rendered("~:C", character),
                character.to_string(),
                "U+{code:04X}"
            );
            assert_eq!(
                rendered("~:@C", character),
                character.to_string(),
                "U+{code:04X}"
            );
        }
    }
    for character in [' ', '\t', '\n', '\r', '\0'] {
        let output = rendered("~:C", character);
        assert!(
            output.len() > 1,
            "named nonprinting character {character:?}: {output:?}"
        );
        assert_eq!(rendered("~C", character), character.to_string());
    }
    // Reader syntax still needs names for these whitespace characters.
    assert_eq!(rendered("~@C", '\u{a0}'), "#\\NO-BREAK_SPACE");
    assert_eq!(rendered("~@C", '\u{3000}'), "#\\IDEOGRAPHIC_SPACE");
    assert!(format_to_layout("~:C", &[EgclVal::from_fixnum(97)]).is_err());
    assert!(format_to_layout("~:C", &[]).is_err());
}
