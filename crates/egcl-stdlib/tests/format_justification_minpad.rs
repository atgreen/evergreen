// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use egcl_stdlib::format::{LayoutOptions, format_to_layout, render_layout};

fn render(control: &str) -> String {
    let doc = format_to_layout(control, &[]).expect("valid justification");
    render_layout(
        &doc,
        &LayoutOptions {
            pretty: false,
            ..Default::default()
        },
    )
}

#[test]
fn single_segment_minpad_does_not_widen_the_implicit_right_justification_slot() {
    let long = "x".repeat(32);
    let short = "x".repeat(20);
    for (control, expected) in [
        (format!("~24,,7<{long}~>"), long.clone()),
        (format!("~24,,7<{short}~>"), format!("    {short}")),
        (format!("~13,,2<{}~>", "x".repeat(30)), "x".repeat(30)),
        (format!("~24<{long}~>"), long),
    ] {
        assert_eq!(render(&control), expected, "{control}");
    }
}

#[test]
fn single_segment_field_rounding_and_character_width_ignore_minpad() {
    for text in [
        "",
        "x",
        "λλ",
        "abc",
        "xxxxxxxxxxxxxxxxxxxx",
        "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
    ] {
        for mincol in [0usize, 2, 5, 24] {
            for colinc in [1usize, 3, 5] {
                let length = text.chars().count();
                let width = if length <= mincol {
                    mincol
                } else {
                    mincol + (length - mincol).div_ceil(colinc) * colinc
                };
                let expected = format!("{}{text}", "*".repeat(width - length));
                for minpad in [0, 2, 7] {
                    let control = format!("~{mincol},{colinc},{minpad},'*<{text}~>");
                    assert_eq!(render(&control), expected, "{control}");
                }
            }
        }
    }
}

#[test]
fn explicit_outer_and_inter_segment_gaps_keep_their_minimum_padding() {
    for (control, expected) in [
        ("~0,,7,'*:<abc~>", "*******abc"),
        ("~0,,7,'*@<abc~>", "abc*******"),
        ("~0,,7,'*:@<abc~>", "*******abc*******"),
        ("~5,3,7,'*:<abc~>", "********abc"),
        ("~5,3,7,'*@<abc~>", "abc********"),
        ("~5,3,7,'*:@<abc~>", "*******abc*******"),
        ("~0,,7,'*<ab~;cd~>", "ab*******cd"),
        ("~0,,7,'*:<ab~;cd~>", "*******ab*******cd"),
        ("~0,,7,'*@<ab~;cd~>", "ab*******cd*******"),
        ("~0,,7,'*:@<ab~;cd~>", "*******ab*******cd*******"),
        ("~0,,7,'*<~>", ""),
        ("~0,,7,'*:<~>", "*******"),
        ("~0,,7,'*@<~>", "*******"),
        ("~0,,7,'*:@<~>", "**************"),
        ("~0,,7,'*<~;~>", "*******"),
    ] {
        assert_eq!(render(control), expected, "{control}");
    }
}

#[test]
fn overflow_prefix_uses_the_corrected_single_segment_field_width() {
    let text = "x".repeat(20);
    assert_eq!(
        render(&format!("~24,,7<BREAK~0,24:;{text}~>")),
        format!("    {text}")
    );
    assert_eq!(
        render(&format!("P:~24,,7<BREAK~0,24:;{text}~>")),
        format!("P:BREAK    {text}")
    );
    // A premature escape leaves the already completed segment as the lone
    // element. The discarded segment must not manufacture a minpad gap.
    assert_eq!(render("~5,,7<abc~;discard~0^~>"), "  abc");
}
