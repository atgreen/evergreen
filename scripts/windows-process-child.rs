// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

// Test fixture for Windows argv quoting, Unicode and concurrent pipe capture.
use std::io::{Read, Write};
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("pipes") {
        let writer = std::thread::spawn(|| {
            std::io::stderr().write_all(&vec![b'E'; 131_072]).unwrap();
        });
        std::io::stdout().write_all(&vec![b'O'; 131_072]).unwrap();
        writer.join().unwrap();
        // Synchronous capture supplies EOF on stdin.
        let mut input = Vec::new();
        std::io::stdin().read_to_end(&mut input).unwrap();
        assert!(input.is_empty());
    } else {
        for argument in args { println!("{}:{}", argument.len(), argument); }
    }
    std::process::exit(23);
}
