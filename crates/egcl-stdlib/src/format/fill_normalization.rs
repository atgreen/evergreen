// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Lexical automatic-fill normalization, before compatibility validation.
use egcl_rt::EgclError;
use std::borrow::Cow;

struct Token {
    start: usize,
    end: usize,
    code: char,
    colon: bool,
    at: bool,
}
struct Frame {
    open: usize,
    separators: Vec<usize>,
}
fn error(message: &str) -> EgclError {
    EgclError::ControlError(message.into())
}

fn tokens(chars: &[char]) -> Result<Vec<Token>, EgclError> {
    let mut result = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '~' {
            i += 1;
            continue;
        }
        let start = i;
        i += 1;
        let mut colon = false;
        let mut at = false;
        while let Some(&ch) = chars.get(i) {
            match ch {
                '\'' => {
                    if i + 1 >= chars.len() {
                        return Err(error("missing quoted FORMAT parameter"));
                    }
                    i += 2;
                }
                ':' => {
                    colon = true;
                    i += 1;
                }
                '@' => {
                    at = true;
                    i += 1;
                }
                ',' | 'v' | 'V' | '#' | '-' | '+' | '0'..='9' => i += 1,
                _ => break,
            }
        }
        let Some(&code) = chars.get(i) else {
            return Err(error("missing FORMAT directive"));
        };
        i += 1;
        if code == '/' {
            while i < chars.len() && chars[i] != '/' {
                i += 1;
            }
            if i == chars.len() {
                return Err(error("unterminated FORMAT function name"));
            }
            i += 1;
        }
        if code == '\n' && !colon {
            // These characters are part of the ignored-newline operation,
            // not retained literal text (including tabs and extra newlines).
            while i < chars.len() && chars[i].is_whitespace() {
                i += 1;
            }
        }
        result.push(Token {
            start,
            end: i,
            code: code.to_ascii_uppercase(),
            colon,
            at,
        });
    }
    Ok(result)
}

/// Fill only literal body text of closing-:@> logical blocks. The retained
/// lexical body includes nested directives and their literal metadata. Neither
/// arguments nor controls obtained later through indirection are inspected.
pub(super) fn normalize(control: &str) -> Result<Cow<'_, str>, EgclError> {
    // No angle block means no automatic fill or logical-block metadata.
    // The downstream compatibility validator still handles ordinary syntax.
    if !control.contains('<') {
        return Ok(Cow::Borrowed(control));
    }
    let chars: Vec<char> = control.chars().collect();
    let tokens = tokens(&chars)?;
    let mut stack: Vec<Frame> = Vec::new();
    let mut fill = Vec::new();
    let mut metadata = Vec::new();
    for (index, token) in tokens.iter().enumerate() {
        match token.code {
            '<' | '(' | '[' | '{' => stack.push(Frame {
                open: index,
                separators: Vec::new(),
            }),
            ';' => {
                if let Some(frame) = stack.last_mut()
                    && tokens[frame.open].code == '<'
                {
                    frame.separators.push(index);
                }
            }
            '>' | ')' | ']' | '}' => {
                let expected = match token.code {
                    '>' => '<',
                    ')' => '(',
                    ']' => '[',
                    _ => '{',
                };
                let Some(frame) = stack.pop() else {
                    return Err(error("unmatched FORMAT closing directive"));
                };
                let open = &tokens[frame.open];
                if open.code != expected {
                    return Err(error("mismatched FORMAT closing directive"));
                }
                if expected != '<' || !token.colon {
                    continue;
                }
                if frame.separators.len() > 2 {
                    return Err(error("too many FORMAT logical-block segments"));
                }
                let body_start = frame
                    .separators
                    .first()
                    .map_or(open.end, |&i| tokens[i].end);
                let body_end = frame
                    .separators
                    .get(1)
                    .map_or(token.start, |&i| tokens[i].start);
                if token.at {
                    fill.push((body_start, body_end));
                }
                if let Some(&first) = frame.separators.first() {
                    metadata.push((open.end, tokens[first].start));
                }
                if let Some(&last) = frame.separators.get(1) {
                    metadata.push((tokens[last].end, token.start));
                }
            }
            _ => {}
        }
    }
    if !stack.is_empty() {
        return Err(error("unclosed FORMAT directive"));
    }
    // Prefixes/suffixes must be constant strings. This also rejects a generated
    // fill directive in nested metadata rather than executing it as plain text.
    metadata.sort_unstable();
    for &(start, end) in &metadata {
        let next = tokens.partition_point(|token| token.start < start);
        if tokens.get(next).is_some_and(|token| token.start < end) {
            return Err(error(
                "FORMAT logical-block prefix/suffix must be constant strings",
            ));
        }
    }
    if fill.is_empty() {
        return Ok(Cow::Borrowed(control));
    }
    // Each enclosing automatic-fill parse transforms retained literal tokens.
    // Preserve multiplicity when both outer and inner logical blocks request
    // fill; do not assume adjacent generated fill events are interchangeable.
    let mut boundaries = Vec::with_capacity(fill.len() * 2);
    for (start, end) in fill {
        if start < end {
            boundaries.push((start, true));
            boundaries.push((end, false));
        }
    }
    boundaries.sort_unstable();
    let mut output = String::with_capacity(control.len());
    let mut position = 0;
    let mut boundary = 0;
    let mut active = 0usize;
    let mut meta = 0;
    let mut previous_newline = false;
    for token in tokens.iter().chain(std::iter::once(&Token {
        start: chars.len(),
        end: chars.len(),
        code: '\0',
        colon: false,
        at: false,
    })) {
        let mut leading = previous_newline;
        while position < token.start {
            while boundary < boundaries.len() && boundaries[boundary].0 <= position {
                if boundaries[boundary].1 {
                    active += 1;
                } else {
                    active -= 1;
                }
                boundary += 1;
            }
            if chars[position] == ' ' {
                let start = position;
                while position < token.start && chars[position] == ' ' {
                    output.push(' ');
                    position += 1;
                }
                if active != 0 && !leading {
                    while meta < metadata.len() && start >= metadata[meta].1 {
                        meta += 1;
                    }
                    if meta < metadata.len() && start >= metadata[meta].0 {
                        return Err(error(
                            "automatic fill inserts a directive into nested logical-block metadata",
                        ));
                    }
                    for _ in 0..active {
                        output.push_str("~:_");
                    }
                }
            } else {
                output.push(chars[position]);
                position += 1;
                leading = false;
            }
        }
        output.extend(chars[token.start..token.end].iter().copied());
        position = token.end;
        previous_newline = token.code == '\n';
    }
    Ok(Cow::Owned(output))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fills_flat_lexical_body_and_preserves_opaque_directive_payloads() {
        assert_eq!(
            normalize("~<pre fix~;a  b ~{c d~}~;suf fix~:@>").unwrap(),
            "~<pre fix~;a  ~:_b ~:_~{c ~:_d~}~;suf fix~:@>"
        );
        assert_eq!(
            normalize("~<~(a b~)~:[c d~;e f~]~:@>").unwrap(),
            "~<~(a ~:_b~)~:[c ~:_d~;e ~:_f~]~:@>"
        );
        assert_eq!(
            normalize("~<~' A~/NAME WITH SPACE/~?~:@>").unwrap(),
            "~<~' A~/NAME WITH SPACE/~?~:@>"
        );
        assert_eq!(
            normalize("~<a~:\n    b c~:@>").unwrap(),
            "~<a~:\n    b ~:_c~:@>"
        );
    }
    #[test]
    fn nested_metadata_and_justification_obey_generated_directive_rules() {
        for control in ["~<~<pre fix~;body~:>~:@>", "~<~<body~;rest~;suf fix~:>~:@>"] {
            assert!(normalize(control).is_err(), "{control}");
        }
        let control = normalize("~<~<a b~>~:@>").unwrap();
        assert!(super::super::control_validation::validate(&control).is_err());
        assert!(normalize("~<own pre fix~;body~;own suf fix~:@>").is_ok());
        assert_eq!(normalize("~<a b~:>").unwrap(), "~<a b~:>");
        assert_eq!(
            normalize("~<~<a b~:@>~:@>").unwrap(),
            "~<~<a ~:_~:_b~:@>~:@>"
        );
    }
}
