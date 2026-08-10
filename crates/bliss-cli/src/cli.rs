//! CLI entry point — argument parsing, REPL driver, and image-load entry.
//!
//! See spec §6.1 (REPL), §7.4 (deployment modes), §2.8 (CLI args).

use bliss_rt::error::BlissError;

// ── CLI arguments ──────────────────────────────────────────────────

/// Parsed CLI arguments.
#[derive(Clone, Debug)]
pub struct CliArgs {
    /// Path to the boot image (--image).
    pub image: Option<String>,
    /// Expression to evaluate and exit (--eval / -e).
    pub eval: Option<String>,
    /// File to load and exit (--load).
    pub load: Option<String>,
    /// Start without an image (--no-image).
    pub no_image: bool,
    /// Bootstrap from lib/boot.lisp (--bootstrap).
    pub bootstrap: bool,
    /// Number of worker threads (--workers).
    pub workers: Option<usize>,
    /// Heap size (--heap-size).
    pub heap_size: Option<String>,
    /// Print help and exit (--help).
    pub help: bool,
    /// Print version and exit (--version).
    pub version: bool,
    /// Enable sandbox mode (--sandbox).
    pub sandbox: bool,
    /// Skip loading init file (--no-init).
    pub no_init: bool,
    /// Arguments passed through to CL (after --).
    pub cl_args: Vec<String>,
    /// Positional argument: script file to execute.
    pub script: Option<String>,
}

/// Helper: take the next value from args for a flag that requires one.
fn take_value<'a>(
    flag: &str,
    iter: &mut impl Iterator<Item = &'a String>,
) -> Result<String, BlissError> {
    iter.next()
        .map(|s| s.clone())
        .ok_or_else(|| BlissError::Internal(format!("{} requires a value", flag)))
}

impl CliArgs {
    /// Parse CLI arguments from a string slice.
    pub fn parse(args: &[String]) -> Result<Self, BlissError> {
        let mut result = CliArgs {
            image: None,
            eval: None,
            load: None,
            no_image: false,
            bootstrap: false,
            workers: None,
            heap_size: None,
            help: false,
            version: false,
            sandbox: false,
            no_init: false,
            cl_args: Vec::new(),
            script: None,
        };

        let mut iter = args.iter();

        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--" => {
                    // Everything after -- is passed through as CL args.
                    result.cl_args = iter.cloned().collect();
                    break;
                }
                "--help" => result.help = true,
                "--version" => result.version = true,
                "--eval" | "-e" => {
                    result.eval = Some(take_value(arg, &mut iter)?);
                }
                "--load" => {
                    result.load = Some(take_value(arg, &mut iter)?);
                }
                "--image" => {
                    result.image = Some(take_value(arg, &mut iter)?);
                }
                "--no-image" => result.no_image = true,
                "--bootstrap" => result.bootstrap = true,
                "--sandbox" => result.sandbox = true,
                "--no-init" => result.no_init = true,
                "--workers" => {
                    let val = take_value(arg, &mut iter)?;
                    let n = val.parse::<usize>().map_err(|_| {
                        BlissError::Internal(format!(
                            "--workers requires a numeric value, got: {}",
                            val
                        ))
                    })?;
                    result.workers = Some(n);
                }
                "--heap-size" => {
                    result.heap_size = Some(take_value(arg, &mut iter)?);
                }
                s if s.starts_with('-') => {
                    return Err(BlissError::Internal(format!("unknown flag: {}", s)));
                }
                _ => {
                    // Positional argument: script file.
                    result.script = Some(arg.clone());
                }
            }
        }

        // ── Conflict checks ──────────────────────────────────────────
        if result.image.is_some() && result.no_image {
            return Err(BlissError::Internal(
                "--image and --no-image are contradictory".into(),
            ));
        }
        if result.sandbox && result.no_image {
            return Err(BlissError::Internal(
                "--sandbox and --no-image are contradictory".into(),
            ));
        }
        if result.no_init && result.bootstrap {
            return Err(BlissError::Internal(
                "--no-init and --bootstrap are contradictory".into(),
            ));
        }
        if result.eval.is_some() && result.load.is_some() {
            return Err(BlissError::Internal(
                "--eval and --load are contradictory".into(),
            ));
        }

        Ok(result)
    }
}

// ── CLI driver ─────────────────────────────────────────────────────

/// Run the CLI: parse args, init runtime, dispatch to REPL/eval/load/script.
pub fn run(args: &[String]) -> Result<i32, BlissError> {
    let cli_args = CliArgs::parse(args)?;

    // Handle informational flags first.
    if cli_args.help {
        print_help();
        return Ok(0);
    }
    if cli_args.version {
        print_version();
        return Ok(0);
    }

    // Dispatch based on mode.
    if let Some(ref expr) = cli_args.eval {
        // --eval / -e: evaluate expression and exit.
        // For now, print what would be evaluated; full compiler integration
        // will replace this with actual evaluation.
        eprintln!("eval: {}", expr);
        return Ok(0);
    }

    if let Some(ref path) = cli_args.load {
        // --load: load file and exit.
        eprintln!("load: {}", path);
        return Ok(0);
    }

    if let Some(ref script) = cli_args.script {
        // Positional script file: load and execute.
        eprintln!("script: {}", script);
        return Ok(0);
    }

    // No eval/load/script — start interactive REPL.
    run_repl()
}

/// Print usage/help text to stdout.
pub fn print_help() {
    println!("Usage: bliss [OPTIONS] [SCRIPT] [-- CL-ARGS...]");
    println!();
    println!("Bliss Common Lisp");
    println!();
    println!("Options:");
    println!("  --help               Print this help message and exit");
    println!("  --version            Print version information and exit");
    println!("  --eval, -e EXPR      Evaluate EXPR and exit");
    println!("  --load FILE          Load FILE and exit");
    println!("  --image FILE         Path to the boot image");
    println!("  --no-image           Start without loading an image");
    println!("  --bootstrap          Bootstrap from lib/boot.lisp");
    println!("  --workers N          Number of worker threads");
    println!("  --heap-size SIZE     Heap size (e.g. 512M, 1G)");
    println!("  --sandbox            Enable sandbox mode");
    println!("  --no-init            Skip loading the init file");
    println!();
    println!("Arguments after -- are passed through to CL as *command-line-args*.");
}

/// Print version information to stdout.
pub fn print_version() {
    println!("bliss {}", env!("CARGO_PKG_VERSION"));
}

// ── REPL driver ────────────────────────────────────────────────────

/// Initialize and run the interactive REPL.
/// Sets up line editing, history, and completion.
pub fn run_repl() -> Result<i32, BlissError> {
    let _config = ReplConfig::default();

    // Print a welcome banner.
    println!("Bliss Common Lisp {}", env!("CARGO_PKG_VERSION"));
    println!("Type (quit) to exit.");
    println!();

    let stdin = std::io::stdin();
    let mut input = String::new();

    loop {
        // Print prompt.
        eprint!("BLISS> ");

        // Read a line from stdin.
        input.clear();
        match stdin.read_line(&mut input) {
            Ok(0) => {
                // EOF — exit cleanly.
                println!();
                return Ok(0);
            }
            Ok(_) => {
                let trimmed = input.trim();
                if trimmed.is_empty() {
                    continue;
                }
                // Check for quit commands.
                if trimmed == "(quit)" || trimmed == "(exit)" {
                    return Ok(0);
                }
                // Echo the form back for now; full eval integration will replace this.
                println!("; => {}", trimmed);
            }
            Err(e) => {
                return Err(BlissError::Internal(format!("read error: {}", e)));
            }
        }
    }
}

/// REPL configuration.
#[derive(Clone, Debug)]
pub struct ReplConfig {
    /// Path to history file (default: ~/.bliss/repl-history).
    pub history_file: String,
    /// Maximum history entries.
    pub history_size: usize,
    /// Enable syntax highlighting.
    pub syntax_highlighting: bool,
}

impl Default for ReplConfig {
    fn default() -> Self {
        Self {
            history_file: "~/.bliss/repl-history".to_string(),
            history_size: 1000,
            syntax_highlighting: true,
        }
    }
}
