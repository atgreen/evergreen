// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::io::{self, Write};

pub(super) fn write_entry(
    output: &mut impl Write,
    tier: &str,
    addr: usize,
    size: usize,
    raw_name: &str,
) -> io::Result<()> {
    // Keep each name a single token, including when a Lisp name contains
    // Unicode whitespace or a newline that would otherwise create a record.
    let name: String = raw_name
        .chars()
        .map(|c| if c.is_whitespace() { '_' } else { c })
        .collect();
    writeln!(output, "{addr:x} {size:x} {tier}:{name}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_preserve_native_tier_and_code_range() {
        let mut output = Vec::new();
        for (tier, addr, size) in [
            ("T1", 0x1000, 0x2a),
            ("T2", 0x2000, 0x39),
            ("OSR", 0x3000, 0x40),
        ] {
            write_entry(&mut output, tier, addr, size, "SQ").unwrap();
        }
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "1000 2a T1:SQ\n2000 39 T2:SQ\n3000 40 OSR:SQ\n"
        );
    }

    #[test]
    fn names_cannot_split_or_inject_records() {
        let mut output = Vec::new();
        write_entry(
            &mut output,
            "T2",
            0x123a,
            0x10,
            "λ name\twith\nspace\u{2003}",
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "123a 10 T2:λ_name_with_space_\n"
        );
    }

    #[test]
    fn writer_errors_propagate() {
        let mut too_short = &mut [0u8; 1][..];
        let error = write_entry(&mut too_short, "T1", 0x1000, 0x10, "SQ").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::WriteZero);
    }
}
