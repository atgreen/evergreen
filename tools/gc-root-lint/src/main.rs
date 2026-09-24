//! gc-root-lint — flag bare `TorclVal` locals read across allocating calls
//! (bliss-jaf; docs/design/gc-rooting.md Part B rung 1).
//!
//! torcl has a precise MOVING minor GC: any allocation can relocate heap
//! objects, so a `TorclVal` held in a plain Rust local across an allocating
//! call is a stale pointer afterwards unless the local is rooted. This lint
//! finds the smell mechanically:
//!
//!   a candidate local (one bound from a known heap-value producer, or
//!   explicitly typed `TorclVal`) that is READ in a statement AFTER an
//!   intervening statement that contains a known-ALLOCATING call, without
//!   having been rooted (`rooted!`/`rooted_ref!`/`StackRoot`/`HostRoot`/
//!   `ShadowRootScope::root`) or reassigned in between.
//!
//! It is deliberately UNSOUND-BUT-USEFUL (see the design doc): scope analysis
//! is per-block and coarse for control flow, and it cannot see types, so
//! immediates (symbols, fixnums) bound from producer calls will false-positive.
//! CI therefore runs it in RATCHET mode: findings are compared against a
//! checked-in baseline (`tools/gc-root-lint/baseline.txt`) and only NEW
//! findings fail the build.
//!
//! Usage:
//!   cargo run -p gc-root-lint                  # report all findings
//!   cargo run -p gc-root-lint -- --check       # fail on findings not in baseline
//!   cargo run -p gc-root-lint -- --bless       # rewrite the baseline
//!
//! A finding is keyed `file:function:variable` (NOT line numbers), so
//! unrelated edits don't churn the baseline.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use syn::spanned::Spanned;
use syn::visit::Visit;

/// Calls that can ALLOCATE on the GC heap (directly or by running Lisp code)
/// and therefore can fire a relocating minor GC. Matched against the last path
/// segment / method name of every call in a statement.
const ALLOCATING: &[&str] = &[
    // allocation primitives
    "alloc_typed",
    "alloc_cons",
    "arena_cons",
    "arena_str",
    "alloc_string",
    "alloc_vector",
    "alloc_ratio",
    "alloc_complex",
    "alloc_bit_vector",
    "alloc_pathname",
    "alloc_structure",
    "alloc_readtable",
    "alloc_stream",
    "make_list",
    "vec_to_cons",
    "vec_to_list",
    "form_list",
    "build_list",
    // interning (allocates SymbolData + name on first intern)
    "intern",
    "intern_symbol",
    "resolve_sym",
    "make_symbol",
    "make_uninterned_symbol",
    // evaluation / expansion (runs arbitrary Lisp -> allocates)
    "eval_form",
    "eval_progn",
    "eval_list",
    "eval_toplevel",
    "apply_function",
    "macroexpand",
    "macroexpand_1",
    "macroexpand_all",
    "compiler_macroexpand_1",
    "expand_body",
    "expand_special_form",
    "expand_function_call_args",
    "walk_cons",
    "macroexpand_environment_from_cli",
    "freeze_env_frame",
    "read_token_with_base",
    "read_list_with_base",
    "read_form_at",
    "lower_expr",
    "lower_expr_inner",
    "compile_thunk",
    "compile_function",
    "run_macro",
    "eval_quasiquote_depth",
    "eval_local_macro_form",
    "eval_local_macro_body",
    "expand_local_macro_call",
    "bind_macrolet_lambda_list",
];

/// Producers whose results are (or may be) MOVABLE heap `TorclVal`s. A `let`
/// binding initialized from one of these becomes a lint candidate. Immediates
/// (fixnum/symbol constructors) are intentionally NOT here.
const PRODUCERS: &[&str] = &[
    "cp",
    "cons_car",
    "cons_cdr",
    "arena_cons",
    "alloc_cons",
    "arena_str",
    "eval_form",
    "macroexpand_all",
    "walk_cons",
    "list_to_vec",
    "cons_to_vec",
    "vec_to_cons",
    "make_list",
];

/// A rooted/whitelisted use: passing the variable to any of these clears it.
const ROOTERS: &[&str] = &[
    "rooted",
    "rooted_ref",
    "new_unlinked",
    "root",
    "root_values",
];
const ROOTER_TYPES: &[&str] = &[
    "StackRoot",
    "HostRoot",
    "VecRootGuard",
    "Rooted",
    "RootedRef",
];

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Finding {
    file: String,
    function: String,
    variable: String,
    line: usize,
    alloc_line: usize,
    allocator: String,
}

impl Finding {
    /// Stable key: no line numbers, so edits elsewhere don't churn the baseline.
    fn key(&self) -> String {
        format!("{}:{}:{}", self.file, self.function, self.variable)
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let check = args.iter().any(|a| a == "--check");
    let bless = args.iter().any(|a| a == "--bless");

    let root = repo_root();
    let mut findings: Vec<Finding> = Vec::new();
    for dir in [
        "crates/torcl/src",
        "crates/torcl-compiler/src",
        "crates/torcl-stdlib/src",
        "crates/torcl-rt/src",
    ] {
        walk_dir(&root.join(dir), &root, &mut findings);
    }
    findings.sort();

    let baseline_path = root.join("tools/gc-root-lint/baseline.txt");
    if bless {
        let keys: BTreeSet<String> = findings.iter().map(Finding::key).collect();
        let mut out = String::from(
            "# gc-root-lint baseline (bliss-jaf). Keys are file:function:variable.\n\
             # Regenerate with: cargo run -p gc-root-lint -- --bless\n",
        );
        for k in &keys {
            out.push_str(k);
            out.push('\n');
        }
        std::fs::write(&baseline_path, out).expect("write baseline");
        println!(
            "blessed {} findings into {}",
            keys.len(),
            baseline_path.display()
        );
        return;
    }

    let baseline: BTreeSet<String> = std::fs::read_to_string(&baseline_path)
        .map(|s| {
            s.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();

    let mut fresh = 0usize;
    for f in &findings {
        let known = baseline.contains(&f.key());
        if check && known {
            continue;
        }
        if !known {
            fresh += 1;
        }
        println!(
            "{}{}:{} [{}] `{}` read after allocating call `{}` (line {}) without rooting",
            if known { "" } else { "NEW " },
            f.file,
            f.line,
            f.function,
            f.variable,
            f.allocator,
            f.alloc_line,
        );
    }
    eprintln!(
        "gc-root-lint: {} findings total, {} not in baseline",
        findings.len(),
        fresh
    );
    if check && fresh > 0 {
        eprintln!(
            "gc-root-lint: FAIL — new unrooted-across-alloc candidates (root them or bless the baseline)"
        );
        std::process::exit(1);
    }
}

fn repo_root() -> PathBuf {
    let mut dir = std::env::current_dir().expect("cwd");
    loop {
        if dir.join("Cargo.toml").exists() && dir.join("crates").exists() {
            return dir;
        }
        if !dir.pop() {
            panic!("run from within the torcl repo");
        }
    }
}

fn walk_dir(dir: &Path, root: &Path, findings: &mut Vec<Finding>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_dir(&path, root, findings);
        } else if path.extension().is_some_and(|e| e == "rs") {
            lint_file(&path, root, findings);
        }
    }
}

fn lint_file(path: &Path, root: &Path, findings: &mut Vec<Finding>) {
    let Ok(source) = std::fs::read_to_string(path) else {
        return;
    };
    let Ok(ast) = syn::parse_file(&source) else {
        return; // unparsable (shouldn't happen for a building tree)
    };
    let rel = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned();
    let mut visitor = FnVisitor {
        file: rel,
        findings,
        current_fn: String::new(),
    };
    visitor.visit_file(&ast);
}

struct FnVisitor<'a> {
    file: String,
    findings: &'a mut Vec<Finding>,
    current_fn: String,
}

impl<'ast> Visit<'ast> for FnVisitor<'_> {
    fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
        let saved = std::mem::replace(&mut self.current_fn, f.sig.ident.to_string());
        lint_block(&f.block, &self.file, &self.current_fn, self.findings);
        syn::visit::visit_item_fn(self, f);
        self.current_fn = saved;
    }
    fn visit_impl_item_fn(&mut self, f: &'ast syn::ImplItemFn) {
        let saved = std::mem::replace(&mut self.current_fn, f.sig.ident.to_string());
        lint_block(&f.block, &self.file, &self.current_fn, self.findings);
        syn::visit::visit_impl_item_fn(self, f);
        self.current_fn = saved;
    }
}

/// Per-block, statement-ordered analysis. Nested blocks are linted separately
/// by the visitor recursion (each fn body once; nested closures/blocks get a
/// coarse treatment: their contents count as uses/allocs of the enclosing
/// statement, which is conservative for ordering within the outer block).
fn lint_block(block: &syn::Block, file: &str, function: &str, findings: &mut Vec<Finding>) {
    // candidate name -> line bound
    let mut candidates: Vec<(String, usize)> = Vec::new();
    // (allocator name, line) of the most recent allocating statement
    let mut last_alloc: Option<(String, usize)> = None;
    // names cleared by rooting/reassignment after the last alloc
    let mut rooted: BTreeSet<String> = BTreeSet::new();
    let mut reported: BTreeSet<String> = BTreeSet::new();

    for stmt in &block.stmts {
        let stmt_line = stmt.span().start().line;
        let idents = collect_idents(stmt);
        let calls = collect_call_names(stmt);
        let macros = collect_macro_names(stmt);

        // 1. Reads of live candidates after an allocating statement → finding.
        if let Some((alloc_name, alloc_line)) = &last_alloc {
            for (name, _bound_line) in &candidates {
                if reported.contains(name) || rooted.contains(name) {
                    continue;
                }
                // A read is any mention that is not this statement re-rooting it.
                let is_rooting_stmt = macros.iter().any(|m| ROOTERS.contains(&m.as_str()))
                    || calls.iter().any(|c| {
                        ROOTERS.contains(&c.as_str()) || ROOTER_TYPES.contains(&c.as_str())
                    });
                if idents.contains(name) {
                    if is_rooting_stmt {
                        rooted.insert(name.clone());
                    } else {
                        findings.push(Finding {
                            file: file.to_string(),
                            function: function.to_string(),
                            variable: name.clone(),
                            line: stmt_line,
                            alloc_line: *alloc_line,
                            allocator: alloc_name.clone(),
                        });
                        reported.insert(name.clone());
                    }
                }
            }
        } else {
            // Rooting before any alloc clears the candidate for good.
            let is_rooting_stmt = macros.iter().any(|m| ROOTERS.contains(&m.as_str()))
                || calls
                    .iter()
                    .any(|c| ROOTERS.contains(&c.as_str()) || ROOTER_TYPES.contains(&c.as_str()));
            if is_rooting_stmt {
                for (name, _) in &candidates {
                    if idents.contains(name) {
                        rooted.insert(name.clone());
                    }
                }
            }
        }

        // 2. New candidate bindings from producer calls or explicit TorclVal type.
        if let syn::Stmt::Local(local) = stmt {
            let from_producer = calls.iter().any(|c| PRODUCERS.contains(&c.as_str()));
            let typed_torclval = local_is_typed_torclval(local);
            if from_producer || typed_torclval {
                for name in pattern_names(&local.pat) {
                    // (Re)binding resets any prior state for the name.
                    candidates.retain(|(n, _)| n != &name);
                    rooted.remove(&name);
                    reported.remove(&name);
                    candidates.push((name, stmt_line));
                }
            } else {
                // Rebinding to something else stops tracking the name.
                for name in pattern_names(&local.pat) {
                    candidates.retain(|(n, _)| n != &name);
                    rooted.remove(&name);
                    reported.remove(&name);
                }
            }
        }

        // 3. Allocating statement → arms the trap for statements after it.
        if let Some(alloc) = calls.iter().find(|c| ALLOCATING.contains(&c.as_str())) {
            last_alloc = Some((alloc.clone(), stmt_line));
            // Values FIRST bound in or before this same statement are consumed
            // by it, not held across it; only later reads count. (Candidates
            // bound in this statement stay tracked for later statements.)
        }
    }
}

fn local_is_typed_torclval(local: &syn::Local) -> bool {
    if let syn::Pat::Type(t) = &local.pat
        && let syn::Type::Path(p) = &*t.ty
    {
        return p
            .path
            .segments
            .last()
            .is_some_and(|s| s.ident == "TorclVal");
    }
    false
}

fn pattern_names(pat: &syn::Pat) -> Vec<String> {
    let mut out = Vec::new();
    collect_pat_names(pat, &mut out);
    out
}

fn collect_pat_names(pat: &syn::Pat, out: &mut Vec<String>) {
    match pat {
        syn::Pat::Ident(i) => out.push(i.ident.to_string()),
        syn::Pat::Tuple(t) => {
            for p in &t.elems {
                collect_pat_names(p, out);
            }
        }
        syn::Pat::Type(t) => collect_pat_names(&t.pat, out),
        syn::Pat::Reference(r) => collect_pat_names(&r.pat, out),
        _ => {}
    }
}

fn collect_idents(stmt: &syn::Stmt) -> BTreeSet<String> {
    struct V(BTreeSet<String>);
    impl<'ast> Visit<'ast> for V {
        fn visit_expr_path(&mut self, p: &'ast syn::ExprPath) {
            if p.path.segments.len() == 1 {
                self.0.insert(p.path.segments[0].ident.to_string());
            }
            syn::visit::visit_expr_path(self, p);
        }
        // Macro bodies are opaque token streams to syn — without this, a
        // variable mentioned only inside `rooted!(x = ...)` / `rooted_ref!(_g =
        // &mut x)` is invisible and every correctly-rooted site false-positives.
        fn visit_macro(&mut self, m: &'ast syn::Macro) {
            collect_token_idents(m.tokens.clone(), &mut self.0);
            syn::visit::visit_macro(self, m);
        }
    }
    let mut v = V(BTreeSet::new());
    v.visit_stmt(stmt);
    v.0
}

fn collect_token_idents(tokens: proc_macro2::TokenStream, out: &mut BTreeSet<String>) {
    for tree in tokens {
        match tree {
            proc_macro2::TokenTree::Ident(i) => {
                out.insert(i.to_string());
            }
            proc_macro2::TokenTree::Group(g) => collect_token_idents(g.stream(), out),
            _ => {}
        }
    }
}

fn collect_call_names(stmt: &syn::Stmt) -> Vec<String> {
    struct V(Vec<String>);
    impl<'ast> Visit<'ast> for V {
        fn visit_expr_call(&mut self, c: &'ast syn::ExprCall) {
            if let syn::Expr::Path(p) = &*c.func
                && let Some(seg) = p.path.segments.last()
            {
                self.0.push(seg.ident.to_string());
                // Also record the type for Type::method paths (StackRoot::new).
                if p.path.segments.len() >= 2 {
                    self.0
                        .push(p.path.segments[p.path.segments.len() - 2].ident.to_string());
                }
            }
            syn::visit::visit_expr_call(self, c);
        }
        fn visit_expr_method_call(&mut self, m: &'ast syn::ExprMethodCall) {
            self.0.push(m.method.to_string());
            syn::visit::visit_expr_method_call(self, m);
        }
    }
    let mut v = V(Vec::new());
    v.visit_stmt(stmt);
    v.0
}

fn collect_macro_names(stmt: &syn::Stmt) -> Vec<String> {
    struct V(Vec<String>);
    impl<'ast> Visit<'ast> for V {
        fn visit_macro(&mut self, m: &'ast syn::Macro) {
            if let Some(seg) = m.path.segments.last() {
                self.0.push(seg.ident.to_string());
            }
            syn::visit::visit_macro(self, m);
        }
    }
    let mut v = V(Vec::new());
    v.visit_stmt(stmt);
    v.0
}
