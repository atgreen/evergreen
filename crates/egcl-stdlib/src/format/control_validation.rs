// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use egcl_rt::EgclError;

#[derive(Default)]
struct Frame {
    opener: Option<char>,
    pretty: bool,
    overflow: bool,
    first_separator: Option<bool>,
}

fn error(message: &str) -> EgclError {
    EgclError::ControlError(message.into())
}

/// Validate lexical directive compatibility within ONE control string. Dynamic
/// string controls are checked separately, without merging their syntax into
/// the caller's control. Parameters are inspected, never evaluated/consumed.
pub(super) fn validate(control: &str) -> Result<(), EgclError> {
    let chars: Vec<char> = control.chars().collect();
    let mut frames = vec![Frame::default()];
    let mut cursor = 0;
    while cursor < chars.len() {
        if chars[cursor] != '~' {
            cursor += 1;
            continue;
        }
        cursor += 1;
        let mut colon = false;
        while let Some(&ch) = chars.get(cursor) {
            match ch {
                '\'' => {
                    // A quoted parameter is exactly one character, including
                    // ~, :, /, or an opening/closing delimiter.
                    if cursor + 1 >= chars.len() {
                        return Err(error("missing quoted FORMAT parameter"));
                    }
                    cursor += 2;
                }
                ':' => {
                    colon = true;
                    cursor += 1;
                }
                '@' | ',' | 'v' | 'V' | '#' | '-' | '+' | '0'..='9' => cursor += 1,
                _ => break,
            }
        }
        let Some(&code) = chars.get(cursor) else {
            return Err(error("missing FORMAT directive"));
        };
        cursor += 1;
        let code = code.to_ascii_uppercase();
        if code == '/' {
            // Slash-name contents are opaque, not nested format directives.
            while cursor < chars.len() && chars[cursor] != '/' {
                cursor += 1;
            }
            if cursor == chars.len() {
                return Err(error("unterminated FORMAT function name"));
            }
            cursor += 1;
            continue;
        }
        match code {
            '<' | '(' | '[' | '{' => frames.push(Frame {
                opener: Some(code),
                ..Frame::default()
            }),
            '>' | ')' | ']' | '}' => {
                let expected = match code {
                    '>' => '<',
                    ')' => '(',
                    ']' => '[',
                    _ => '{',
                };
                if frames.len() == 1 {
                    return Err(error("unmatched FORMAT closing directive"));
                }
                let mut frame = frames.pop().unwrap();
                if frame.opener != Some(expected) {
                    return Err(error("mismatched FORMAT closing directive"));
                }
                if expected == '<' {
                    if colon {
                        frame.pretty = true;
                    } else {
                        if frame.pretty {
                            return Err(error(
                                "pretty-print directive nested inside FORMAT justification",
                            ));
                        }
                        frame.overflow |= frame.first_separator == Some(true);
                    }
                }
                let parent = frames.last_mut().unwrap();
                parent.pretty |= frame.pretty;
                parent.overflow |= frame.overflow;
            }
            ';' => {
                let frame = frames.last_mut().unwrap();
                if frame.opener == Some('<') && frame.first_separator.is_none() {
                    frame.first_separator = Some(colon);
                }
            }
            'W' | 'I' | '_' => frames.last_mut().unwrap().pretty = true,
            'T' if colon => frames.last_mut().unwrap().pretty = true,
            _ => {}
        }
    }
    if frames.len() != 1 {
        return Err(error("unclosed FORMAT directive"));
    }
    let root = &frames[0];
    if root.pretty && root.overflow {
        return Err(error(
            "pretty-print directive and overflow justification in the same FORMAT control",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_all_pretty_families_and_nested_overflow_independent_of_order() {
        for control in [
            "~<XXX~1,1:TYYY~>",
            "~<XXX~:;YYY~>ZZZ~4,5:tWWW",
            "AAAA~1,1:TBBB~<XXX~:;YYY~>ZZZ",
            "~<~:;~>~<~:>",
            "~<~:>~<~:;~>",
            "1~<X~<Y~:>Z~>2",
            "~<X~_Y~>",
            "~<X~:;Y~>~W",
            "~<X~I~>",
            "~<~(~3:@T~)~>",
            "~<~[ok~;~W~]~>",
            "~<~{~_~}~>",
            "~<~<X~:;Y~>~:>",
            "~(~<X~:;Y~>~)~:@T",
        ] {
            assert!(validate(control).is_err(), "{control}");
        }
    }

    #[test]
    fn preserves_plain_tabs_and_opaque_payloads_and_separate_controls() {
        for control in [
            "~<~4T~3@T~>",
            "~:<plain~>",
            "~<plain~>~W",
            "~<~~:T~>",
            "~<~'~,3A~>",
            "~<~/|NAME~W|/~>",
            "~<~v,vT~>",
            "~<~[~:T~;~W~]~:>",
            "~<~<A~:;B~>~>",
            "~<prefix~;body~:>",
        ] {
            assert!(validate(control).is_ok(), "{control}");
        }
        assert!(validate("~<X~:;Y~>~?").is_ok());
        assert!(validate("~W").is_ok());
    }
}
