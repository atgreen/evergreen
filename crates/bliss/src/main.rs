//! Bliss Common Lisp — entry point.

use bliss::cli;

fn main() {
    // Collect OS args, skipping argv[0] (the program name).
    let args: Vec<String> = std::env::args().skip(1).collect();

    match cli::run(&args) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("bliss: {}", e);
            std::process::exit(1);
        }
    }
}
