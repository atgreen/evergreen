//! Bliss Common Lisp — entry point.

use bliss::cli;

fn main() {
    // Collect OS args, skipping argv[0] (the program name).
    let args: Vec<String> = std::env::args().skip(1).collect();

    match cli::run(&args) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            // Use describe_err so symbols render by name (e.g. "undefined
            // function: FIND-IF") rather than "Symbol(194)".
            eprintln!("bliss: {}", cli::describe_err(&e));
            std::process::exit(1);
        }
    }
}
