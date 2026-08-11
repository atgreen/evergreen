use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

fn bliss() -> Command {
    Command::new(env!("CARGO_BIN_EXE_bliss-cli"))
}

fn temp_dir(name: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before unix epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("bliss-spec-{}-{}", name, unique));
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn write_file(path: &Path, contents: &str) {
    fs::write(path, contents).expect("write temp file");
}

#[test]
fn eval_mode_uses_real_cli_entrypoint_and_overrides_init_discovery() {
    // Per R7.11 and R7.21, explicit CLI mode wins over ambient startup sources.
    // Per R10.06, the acceptance test must drive the real user-facing entrypoint.
    let dir = temp_dir("eval");
    let init = dir.join("init.lisp");
    write_file(&init, "(print 99)\n");

    let output = bliss()
        .env("BLISS_INIT_FILE", &init)
        .args(["--eval", "(+ 1 2)"])
        .output()
        .expect("run bliss --eval");

    assert_eq!(output.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains('3'), "stdout was: {stdout}");
    assert!(!stdout.contains("99"), "explicit --eval should suppress init-file execution: {stdout}");
    fs::remove_dir_all(dir).ok();
}

#[test]
fn load_mode_executes_a_real_file_path() {
    // Per R7.07, Bliss MUST support a file-path load mode.
    // Per R10.06, the end-to-end pipeline must produce observable output.
    let dir = temp_dir("load");
    let script = dir.join("load-test.lisp");
    write_file(&script, "(print (+ 20 22))\n");

    let output = bliss()
        .args(["--load", script.to_str().expect("utf8 path")])
        .output()
        .expect("run bliss --load");

    assert_eq!(output.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("42"),
        "stdout: {} stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    fs::remove_dir_all(dir).ok();
}

#[test]
fn script_mode_executes_the_positional_script_entrypoint() {
    // Per R7.07 and R7.21, a positional script path is a deterministic startup mode.
    let dir = temp_dir("script");
    let script = dir.join("argv-script.lisp");
    write_file(&script, "(print 'script-ran)\n");

    let output = bliss()
        .arg(script.to_str().expect("utf8 path"))
        .output()
        .expect("run bliss script");

    assert_eq!(output.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    assert!(
        String::from_utf8_lossy(&output.stdout).to_uppercase().contains("SCRIPT-RAN"),
        "stdout: {} stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    fs::remove_dir_all(dir).ok();
}

#[test]
fn repl_acceptance_drives_the_real_binary_through_read_eval_print_and_exit() {
    // Per R6.01, the REPL MUST implement ANSI read-eval-print loop semantics.
    // Per R10.06, this acceptance test drives the real CLI through stdin/stdout.
    let mut child = bliss()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn bliss repl");

    {
        let stdin = child.stdin.as_mut().expect("child stdin");
        stdin.write_all(b"(+ 1 2)\n(quit)\n").expect("write repl input");
    }

    let output = child.wait_with_output().expect("wait for repl");
    assert_eq!(output.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stdout.contains("Bliss Common Lisp"), "stdout: {stdout}");
    assert!(stdout.contains('3') || stderr.contains("BLISS>"), "stdout: {stdout} stderr: {stderr}");
}

#[test]
fn no_image_eval_mode_supports_bootstrap_without_a_saved_image() {
    // Per R7.21 and R7.22, --no-image is an explicit startup profile and MUST still permit bootstrapping work.
    let output = bliss()
        .args(["--no-image", "--eval", "(+ 4 5)"])
        .output()
        .expect("run bliss --no-image --eval");

    assert_eq!(output.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    assert!(String::from_utf8_lossy(&output.stdout).contains('9'));
}

#[test]
fn bundled_asdf_can_be_loaded_via_the_real_cli_load_mode() {
    // Per R6.45 and R6.47, Bliss MUST ship and integrate bundled ASDF support.
    let asdf = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../lib/asdf.lisp");
    assert!(asdf.exists(), "bundled ASDF file missing at {}", asdf.display());

    let output = bliss()
        .args(["--load", asdf.to_str().expect("utf8 path")])
        .output()
        .expect("run bliss --load lib/asdf.lisp");

    assert_eq!(output.status.code(), Some(0), "stdout: {} stderr: {}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
}
