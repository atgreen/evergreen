//! Differential fuzzing of the T1 speculative codegen (bliss-jtc.27).
//!
//! The invariant the whole hotspot engine must preserve is: running a function
//! as native T1 code produces exactly what pure interpretation produces — the
//! same value, or the same error. This test generates a large, deterministic
//! batch of random *pure* expressions over every inlined operator (fixnum
//! arithmetic, comparisons, predicates, cons access, EQ/NULL/…), forces them all
//! to T1 with a threshold of 1, and asserts the whole program's output matches
//! the tree-walker's byte for byte.
//!
//! Determinism: a fixed-seed LCG drives generation (no Date/random), so a
//! failure reproduces exactly. Results are normalised so nothing prints a heap
//! address — arithmetic is reduced `mod` a fixnum (bignums, reached via internal
//! overflow + deopt, collapse back to a printable fixnum), and boolean/predicate
//! programs print only T/NIL.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_bliss-cli");

/// Deterministic LCG (SplitMix-ish output scramble). Seeded per run.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn pick<'a>(&mut self, xs: &[&'a str]) -> &'a str {
        xs[self.below(xs.len())]
    }
}

const VARS: &[&str] = &["a", "b", "c"];
const SMALL: &[&str] = &["0", "1", "2", "3", "-1", "-2", "7", "-5", "10"];

/// A pure fixnum-arithmetic expression over the inlined ops.
fn arith(rng: &mut Rng, depth: usize) -> String {
    if depth == 0 || rng.below(3) == 0 {
        return if rng.below(2) == 0 { rng.pick(VARS) } else { rng.pick(SMALL) }.to_string();
    }
    match rng.below(6) {
        0 => format!("(+ {} {})", arith(rng, depth - 1), arith(rng, depth - 1)),
        1 => format!("(- {} {})", arith(rng, depth - 1), arith(rng, depth - 1)),
        2 => format!("(* {} {})", arith(rng, depth - 1), arith(rng, depth - 1)),
        3 => format!("(1+ {})", arith(rng, depth - 1)),
        4 => format!("(1- {})", arith(rng, depth - 1)),
        _ => format!("(- {})", arith(rng, depth - 1)),
    }
}

/// A pure boolean expression: comparisons, fixnum/total predicates, cons access,
/// and and/or/not structure. Always evaluates to T or NIL.
fn boolean(rng: &mut Rng, depth: usize) -> String {
    if depth == 0 || rng.below(2) == 0 {
        // Leaf predicate/comparison.
        return match rng.below(11) {
            0 => format!("(< {} {})", arith(rng, 2), arith(rng, 2)),
            1 => format!("(> {} {})", arith(rng, 2), arith(rng, 2)),
            2 => format!("(<= {} {})", arith(rng, 2), arith(rng, 2)),
            3 => format!("(>= {} {})", arith(rng, 2), arith(rng, 2)),
            4 => format!("(= {} {})", arith(rng, 2), arith(rng, 2)),
            5 => format!("(zerop {})", arith(rng, 2)),
            6 => format!("(evenp {})", arith(rng, 2)),
            7 => format!("(oddp {})", arith(rng, 2)),
            8 => format!("(plusp {})", arith(rng, 2)),
            9 => format!("(minusp {})", arith(rng, 2)),
            _ => {
                // cons/eq/null over list leaves.
                let list = rng.pick(&["nil", "(list a b c)", "(cons a b)", "(list a)"]);
                match rng.below(4) {
                    0 => format!("(null {list})"),
                    1 => format!("(consp {list})"),
                    2 => format!("(atom {list})"),
                    _ => format!("(eq (car {list}) a)"),
                }
            }
        };
    }
    match rng.below(3) {
        0 => format!("(and {} {})", boolean(rng, depth - 1), boolean(rng, depth - 1)),
        1 => format!("(or {} {})", boolean(rng, depth - 1), boolean(rng, depth - 1)),
        _ => format!("(not {})", boolean(rng, depth - 1)),
    }
}

/// Argument tuples, including edge values (zero, negatives, near-overflow).
const ARG_TUPLES: &[&str] = &[
    "0 0 0",
    "3 -7 12",
    "-5 5 -1",
    "1000000000 3 2",
    "1152921504606846975 1 2",     // near +fixnum-max
    "-1152921504606846976 -1 1",   // fixnum-min
    "2 1073741824 2",              // squares overflow
];

fn build_program(seed: u64, count: usize) -> String {
    let mut rng = Rng(seed);
    let mut p = String::new();
    for i in 0..count {
        let numeric = i % 2 == 0;
        let body = if numeric {
            // mod keeps output a fixnum even when the expression overflows.
            format!("(mod {} 1000003)", arith(&mut rng, 4))
        } else {
            boolean(&mut rng, 3)
        };
        p.push_str(&format!("(defun f{i} (a b c) {body})\n"));
        for (j, args) in ARG_TUPLES.iter().enumerate() {
            p.push_str(&format!("(format t \"{i}.{j}=~a~%\" (f{i} {args}))\n"));
        }
    }
    p
}

fn run(program_path: &str, envs: &[(&str, &str)]) -> (String, bool) {
    let mut cmd = Command::new(BIN);
    cmd.args(["--load", program_path]);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("spawn bliss-cli");
    (String::from_utf8_lossy(&out.stdout).into_owned(), out.status.success())
}

#[test]
fn t1_matches_interpreter_on_random_pure_programs() {
    let dir = std::env::temp_dir().join(format!("bliss-t1fuzz-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    // A handful of independent seeds, each a big batch of functions.
    for seed in [1u64, 2, 3, 5, 8, 13, 21, 34, 55, 89] {
        let program = build_program(seed, 160);
        let path = dir.join(format!("fuzz-{seed}.lisp"));
        std::fs::write(&path, &program).expect("write program");
        let p = path.to_str().unwrap();

        let (t1, t1_ok) = run(p, &[("BLISS_T1_THRESHOLD", "1")]);
        let (tw, tw_ok) = run(p, &[("BLISS_BACKEND", "tree-walker")]);
        assert_eq!(t1_ok, tw_ok, "exit status differs (seed {seed})");
        if t1 != tw {
            // Find the first differing line for a compact report.
            let first_diff = t1
                .lines()
                .zip(tw.lines())
                .find(|(a, b)| a != b)
                .map(|(a, b)| format!("T1={a:?}  TW={b:?}"))
                .unwrap_or_else(|| "length differs".to_string());
            panic!("T1 vs tree-walker mismatch (seed {seed}): {first_diff}");
        }
    }
    std::fs::remove_dir_all(&dir).ok();
}
