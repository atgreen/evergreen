//! CLI entry point — argument parsing, REPL driver, and image-load entry.
//! See spec §6.1 (REPL), §7.4 (deployment modes), §2.8 (CLI args).

use bliss_compiler::macroexpand::{
    self as compiler_macroexpand, Environment as MacroexpandEnv, FunctionInfo, VariableInfo,
};
use bliss_compiler::reader;
use bliss_rt::error::BlissError;
use bliss_rt::object::{ConsCell, ObjectHeader, RatioData, type_id};
use bliss_rt::runtime::parse_cli as parse_runtime_cli;
use bliss_rt::value::{BlissVal, EOF, NIL, T};

use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

// ── CLI arguments ──────────────────────────────────────────────────
#[derive(Clone, Debug)]
pub struct CliArgs {
    pub image: Option<String>,
    pub eval: Option<String>,
    pub load: Option<String>,
    pub no_image: bool,
    pub bootstrap: bool,
    pub no_bootstrap: bool,
    pub workers: Option<usize>,
    pub heap_size: Option<String>,
    pub help: bool,
    pub version: bool,
    pub sandbox: bool,
    pub no_init: bool,
    pub cl_args: Vec<String>,
    pub script: Option<String>,
}

impl CliArgs {
    pub fn parse(args: &[String]) -> Result<Self, BlissError> {
        let mut shared_args = Vec::new();
        let mut cl_args = Vec::new();
        let mut bootstrap = false;
        let mut no_bootstrap = false;
        let mut sandbox = false;
        let mut no_init = false;
        let mut help = false;
        let mut version = false;
        let mut script = None;
        let mut saw_double_dash = false;

        let mut i = 0;
        while i < args.len() {
            let arg = &args[i];
            if saw_double_dash {
                cl_args.push(arg.clone());
                i += 1;
                continue;
            }

            match arg.as_str() {
                "--" => {
                    saw_double_dash = true;
                    i += 1;
                }
                "--help" | "--version" => {
                    if arg == "--help" {
                        help = true;
                    } else {
                        version = true;
                    }
                    i += 1;
                }
                s if is_forwarded_runtime_flag(s) => {
                    shared_args.push(arg.clone());
                    if runtime_flag_requires_value(arg) {
                        let value = args.get(i + 1).ok_or_else(|| {
                            BlissError::Internal(format!("{} requires a value", arg))
                        })?;
                        shared_args.push(value.clone());
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                "-e" => {
                    shared_args.push("--eval".into());
                    let value = args
                        .get(i + 1)
                        .ok_or_else(|| BlissError::Internal("-e requires a value".into()))?;
                    shared_args.push(value.clone());
                    i += 2;
                }
                "--bootstrap" => {
                    // The prelude now loads by default; --bootstrap is kept as an
                    // accepted no-op for backward compatibility.
                    bootstrap = true;
                    i += 1;
                }
                "--no-bootstrap" => {
                    no_bootstrap = true;
                    i += 1;
                }
                "--sandbox" => {
                    sandbox = true;
                    i += 1;
                }
                "--no-init" => {
                    no_init = true;
                    i += 1;
                }
                s if s.starts_with('-') => {
                    return Err(BlissError::Internal(format!("unknown flag: {}", s)));
                }
                _ => {
                    if script.is_none() {
                        script = Some(arg.clone());
                    }
                    i += 1;
                }
            }
        }

        let (config, runtime_cl_args) = parse_runtime_cli(&shared_args)?;
        if !runtime_cl_args.is_empty() {
            cl_args.extend(runtime_cl_args);
        }

        let r = CliArgs {
            image: extract_flag_value(&shared_args, "--image"),
            eval: config.eval_form.clone(),
            load: config.load_file.clone(),
            no_image: shared_args.iter().any(|arg| arg == "--no-image"),
            bootstrap,
            no_bootstrap,
            workers: extract_flag_value(&shared_args, "--workers")
                .map(|value| value.parse::<usize>())
                .transpose()
                .map_err(|_| BlissError::Internal("--workers requires a numeric value".into()))?,
            heap_size: extract_size_arg(&shared_args, "--heap-size"),
            help,
            version,
            sandbox,
            no_init,
            cl_args,
            script,
        };
        if r.image.is_some() && r.no_image {
            return Err(BlissError::Internal(
                "--image and --no-image are contradictory".into(),
            ));
        }
        if r.sandbox && r.no_image {
            return Err(BlissError::Internal(
                "--sandbox and --no-image are contradictory".into(),
            ));
        }
        if r.eval.is_some() && r.load.is_some() {
            return Err(BlissError::Internal(
                "--eval and --load are contradictory".into(),
            ));
        }
        Ok(r)
    }
}

fn is_forwarded_runtime_flag(flag: &str) -> bool {
    matches!(
        flag,
        "--eval"
            | "--load"
            | "--image"
            | "--no-image"
            | "--workers"
            | "--heap-size"
            | "--tlab-size"
            | "--nursery-size"
            | "--stack-size"
            | "--gc-log"
            | "--jit-dump"
            | "--log-level"
    )
}

fn runtime_flag_requires_value(flag: &str) -> bool {
    matches!(
        flag,
        "--eval"
            | "--load"
            | "--image"
            | "--workers"
            | "--heap-size"
            | "--tlab-size"
            | "--nursery-size"
            | "--stack-size"
            | "--gc-log"
            | "--log-level"
    )
}

fn extract_size_arg(args: &[String], flag: &str) -> Option<String> {
    extract_flag_value(args, flag)
}

fn extract_flag_value(args: &[String], flag: &str) -> Option<String> {
    args.windows(2).find_map(|pair| {
        if pair[0] == flag {
            Some(pair[1].clone())
        } else {
            None
        }
    })
}

// ── Arena allocator (replaces Box::leak) ─────────────────────────
// A bump allocator that collects all allocations and frees them on drop.
// Uses a generational scheme: blocks can be promoted to a "permanent" set
// that survives arena compaction, while temporary blocks are freed.
struct Arena {
    blocks: Vec<(*mut u8, std::alloc::Layout)>,
    /// Permanent blocks that survive compaction (referenced by the environment)
    permanent: Vec<(*mut u8, std::alloc::Layout)>,
    /// Threshold: when blocks exceed this count, a compaction is suggested
    compaction_threshold: usize,
}

impl Arena {
    fn new() -> Self {
        Arena {
            blocks: Vec::new(),
            permanent: Vec::new(),
            compaction_threshold: 10000,
        }
    }

    /// Promote all current blocks to permanent status.
    /// Call this after loading an image or defining persistent functions
    /// so those allocations survive future compaction.
    fn promote_all(&mut self) {
        self.permanent.append(&mut self.blocks);
    }

    /// Free all non-permanent blocks. This reclaims memory from temporary
    /// allocations (e.g., intermediate eval results in the REPL).
    /// SAFETY: Caller must ensure no references to non-permanent blocks exist.
    fn compact(&mut self) {
        for &(ptr, layout) in &self.blocks {
            unsafe {
                std::alloc::dealloc(ptr, layout);
            }
        }
        self.blocks.clear();
    }

    /// Returns true if the number of temporary blocks exceeds the threshold
    fn should_compact(&self) -> bool {
        self.blocks.len() > self.compaction_threshold
    }

    fn alloc_cons(&mut self, car: BlissVal, cdr: BlissVal) -> BlissVal {
        let layout = std::alloc::Layout::new::<ConsCell>();
        unsafe {
            let ptr = std::alloc::alloc_zeroed(layout);
            if ptr.is_null() {
                std::alloc::handle_alloc_error(layout);
            }
            let cell = ptr as *mut ConsCell;
            (*cell).car = car;
            (*cell).cdr = cdr;
            self.blocks.push((ptr, layout));
            BlissVal::from_cons_ptr(ptr)
        }
    }

    fn alloc_str(&mut self, s: &str) -> BlissVal {
        let b = s.as_bytes();
        let sz = 8 + 8 + b.len();
        let layout = std::alloc::Layout::from_size_align(sz, 8).unwrap();
        unsafe {
            let p = std::alloc::alloc_zeroed(layout);
            if p.is_null() {
                std::alloc::handle_alloc_error(layout);
            }
            *(p as *mut ObjectHeader) =
                ObjectHeader::new(type_id::SIMPLE_BASE_STRING, sz.div_ceil(8) as u16);
            *(p.add(8) as *mut u64) = b.len() as u64;
            std::ptr::copy_nonoverlapping(b.as_ptr(), p.add(16), b.len());
            self.blocks.push((p, layout));
            BlissVal::from_heap_ptr(p)
        }
    }
}

impl Drop for Arena {
    fn drop(&mut self) {
        for &(ptr, layout) in &self.blocks {
            unsafe {
                std::alloc::dealloc(ptr, layout);
            }
        }
        for &(ptr, layout) in &self.permanent {
            unsafe {
                std::alloc::dealloc(ptr, layout);
            }
        }
    }
}

// Thread-local arena so alloc functions can be called from anywhere in the evaluator.
thread_local! {
    static ARENA: RefCell<Arena> = RefCell::new(Arena::new());
}

fn arena_cons(car: BlissVal, cdr: BlissVal) -> BlissVal {
    ARENA.with(|a| a.borrow_mut().alloc_cons(car, cdr))
}

fn arena_str(s: &str) -> BlissVal {
    let val = ARENA.with(|a| a.borrow_mut().alloc_str(s));
    bliss_stdlib::register_string(val, s);
    val
}

// ── Stream tracking for file I/O ─────────────────────────────────
#[derive(Clone, Debug)]
enum StreamDirection {
    Input,
    Output,
}

#[derive(Clone, Debug)]
struct StreamState {
    content: String,
    position: usize,
    direction: StreamDirection,
    path: String,
    buffer: String,
}

thread_local! {
    static STREAMS: RefCell<HashMap<u64, StreamState>> = RefCell::new(HashMap::new());
    static NEXT_STREAM_ID: RefCell<u64> = const { RefCell::new(1) };
}

fn open_stream(path: &str, direction: StreamDirection) -> Result<BlissVal, BlissError> {
    let state = match direction {
        StreamDirection::Input => {
            let content = std::fs::read_to_string(path)
                .map_err(|e| BlissError::FileError(format!("cannot open {}: {}", path, e)))?;
            StreamState {
                content,
                position: 0,
                direction,
                path: path.to_string(),
                buffer: String::new(),
            }
        }
        StreamDirection::Output => StreamState {
            content: String::new(),
            position: 0,
            direction,
            path: path.to_string(),
            buffer: String::new(),
        },
    };
    let id = NEXT_STREAM_ID.with(|c| {
        let v = *c.borrow();
        *c.borrow_mut() = v + 1;
        v
    });
    STREAMS.with(|s| s.borrow_mut().insert(id, state));
    // Represent stream as a tagged fixnum with high bit set to distinguish from regular fixnums
    Ok(BlissVal::from_fixnum(-(id as i64)))
}

fn close_stream(stream_val: BlissVal) -> Result<(), BlissError> {
    if !stream_val.is_fixnum() {
        return Ok(());
    }
    let id = (-stream_val.as_fixnum()) as u64;
    STREAMS.with(|s| {
        if let Some(state) = s.borrow_mut().remove(&id) {
            if matches!(state.direction, StreamDirection::Output) {
                std::fs::write(&state.path, &state.buffer).map_err(|e| {
                    BlissError::FileError(format!("cannot write {}: {}", state.path, e))
                })?;
            }
            Ok(())
        } else {
            Ok(())
        }
    })
}

fn is_stream(val: BlissVal) -> bool {
    val.is_fixnum() && val.as_fixnum() < 0
}

fn stream_read_line(stream_val: BlissVal) -> Result<(String, bool), BlissError> {
    if !is_stream(stream_val) {
        return Err(BlissError::StreamError("not a stream".into()));
    }
    let id = (-stream_val.as_fixnum()) as u64;
    STREAMS.with(|s| {
        let mut streams = s.borrow_mut();
        if let Some(state) = streams.get_mut(&id) {
            if state.position >= state.content.len() {
                return Ok(("".to_string(), true)); // EOF
            }
            let remaining = &state.content[state.position..];
            if let Some(newline_pos) = remaining.find('\n') {
                let line = remaining[..newline_pos].to_string();
                state.position += newline_pos + 1;
                Ok((line, false))
            } else {
                let line = remaining.to_string();
                state.position = state.content.len();
                Ok((line, true))
            }
        } else {
            Err(BlissError::StreamError("stream not open".into()))
        }
    })
}

fn stream_write_string(stream_val: BlissVal, s: &str) -> Result<(), BlissError> {
    if !is_stream(stream_val) {
        return Err(BlissError::StreamError("not a stream".into()));
    }
    let id = (-stream_val.as_fixnum()) as u64;
    STREAMS.with(|streams| {
        let mut streams = streams.borrow_mut();
        if let Some(state) = streams.get_mut(&id) {
            state.buffer.push_str(s);
            Ok(())
        } else {
            Err(BlissError::StreamError("stream not open".into()))
        }
    })
}

// ── Closure representation ───────────────────────────────────────
#[derive(Clone)]
struct Closure {
    /// Raw lambda list, for full &optional/&rest/&key binding.
    params_form: BlissVal,
    body: BlissVal,
    captured_frame: Rc<RefCell<EnvFrame>>,
}

#[derive(Clone, Copy)]
enum EvalContext {
    Repl,
    Eval,
    Load,
}

// ── Environment for variable/function bindings ───────────────────
// Uses Rc for global definitions (funs, macros, classes, methods, packages)
// so that child() only clones the local vars HashMap, not the entire env.
#[derive(Clone)]
struct Env {
    frame: Rc<RefCell<EnvFrame>>,
    funs: Rc<HashMap<String, FunDef>>,
    macros: Rc<HashMap<String, MacroDef>>,
    symbol_macros: Rc<HashMap<u32, BlissVal>>,
    classes: Rc<HashMap<String, ClassDef>>,
    generics: Rc<HashMap<String, GenericDef>>,
    methods: Rc<HashMap<String, Vec<MethodDef>>>,
    packages: Rc<HashMap<String, PackageDef>>,
    current_package: String,
    sandbox: bool,
    restarts: Vec<RestartEntry>,
    handlers: Vec<HandlerEntry>,
    /// Multiple values from last (values ...) or (floor ...) call
    mv: Vec<BlissVal>,
    mv_active: bool,
    /// Closures stored by name or lambda id
    closures: Rc<RefCell<HashMap<u64, Closure>>>,
    block_stack: Vec<(String, String)>,
    catch_stack: Vec<(String, String)>,
    /// Tags visible for GO: (tag-name, tagbody-token)
    tag_stack: Vec<(String, String)>,
    method_context: Vec<MethodContext>,
    eval_context: EvalContext,
}

#[derive(Clone, Default)]
struct EnvFrame {
    vars: HashMap<String, BlissVal>,
    symbol_vars: HashMap<u32, BlissVal>,
    parent: Option<Rc<RefCell<EnvFrame>>>,
}

#[derive(Clone)]
struct FrozenEnvFrame {
    vars: HashMap<String, BlissVal>,
    symbol_vars: HashMap<u32, BlissVal>,
    parent: Option<Arc<FrozenEnvFrame>>,
}

#[derive(Clone)]
struct FunDef {
    params: Vec<String>,
    /// Raw lambda list, for full &optional/&rest/&key binding.
    params_form: BlissVal,
    body: BlissVal,
}

#[derive(Clone)]
struct MacroDef {
    params_form: BlissVal,
    body: BlissVal,
    captured_frame: Rc<RefCell<EnvFrame>>,
}

#[allow(dead_code)]
#[derive(Clone)]
struct ClassDef {
    name: String,
    supers: Vec<String>,
    slots: Vec<SlotDef>,
    class_slot_values: Arc<Mutex<HashMap<String, Option<BlissVal>>>>,
}

#[derive(Clone)]
struct SlotDef {
    name: String,
    initarg: Option<String>,
    accessor: Option<String>,
    readers: Vec<String>,
    writers: Vec<String>,
    initform: Option<BlissVal>,
    allocation: SlotAllocation,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SlotAllocation {
    Instance,
    Class,
}

#[derive(Clone)]
struct MethodDef {
    method_id: BlissVal,
    specializers: Vec<MethodSpecializer>,
    params: Vec<String>,
    body: BlissVal,
}

#[derive(Clone)]
struct GenericDef {
    generic_function: BlissVal,
    combination: bliss_stdlib::MethodCombinationType,
}

#[derive(Clone)]
enum MethodSpecializer {
    Any,
    Class(String),
    Eql(BlissVal),
}

#[derive(Clone)]
struct MethodContext {
    args: Vec<BlissVal>,
    next: NextMethod,
}

#[derive(Clone)]
enum NextMethod {
    Standard {
        around: Vec<MethodDef>,
        before: Vec<MethodDef>,
        primary: Vec<MethodDef>,
        after: Vec<MethodDef>,
    },
    Primary {
        primary: Vec<MethodDef>,
    },
}

#[allow(dead_code)]
#[derive(Clone)]
struct PackageDef {
    name: String,
    exports: Vec<String>,
    uses: Vec<String>,
    symbols: HashMap<String, BlissVal>,
}

#[derive(Clone)]
struct RestartEntry {
    name: String,
    function: RestartFunction,
    interactive_function: Option<RestartFunction>,
    test_function: Option<RestartFunction>,
    unwind_on_invoke: bool,
}

#[derive(Clone)]
enum RestartFunction {
    FunctionForm {
        function_form: BlissVal,
        captured_frame: Arc<FrozenEnvFrame>,
    },
    ContinueNil,
}

/// Sentinel error used for non-local control flow when invoke-restart is called.
#[derive(Debug)]
#[allow(dead_code)]
struct RestartInvoked {
    name: String,
    result: BlissVal,
}

#[derive(Clone)]
struct HandlerEntry {
    type_name: String,
    handler: HandlerImpl,
}

#[derive(Clone)]
enum HandlerImpl {
    Function(BlissVal),
    HandlerCase {
        token: String,
        var_name: Option<String>,
        body: BlissVal,
        captured_frame: Arc<FrozenEnvFrame>,
    },
}

thread_local! {
    static CONTROL_VALUES: RefCell<HashMap<String, BlissVal>> = RefCell::new(HashMap::new());
    static CONTROL_COUNTER: RefCell<u64> = const { RefCell::new(0) };
    static MACROEXPAND_ENVIRONMENTS: RefCell<HashMap<u64, MacroexpandEnv>> = RefCell::new(HashMap::new());
    static NEXT_MACROEXPAND_ENVIRONMENT_ID: RefCell<u64> = const { RefCell::new(1) };
}

static MACRO_FUNCTION_HANDLE_COUNTER: AtomicU64 = AtomicU64::new(1);

fn next_control_token(prefix: &str) -> String {
    let id = CONTROL_COUNTER.with(|counter| {
        let id = *counter.borrow();
        *counter.borrow_mut() = id + 1;
        id
    });
    format!("{prefix}:{id}")
}

fn next_macro_function_handle() -> BlissVal {
    BlissVal::from_fixnum(
        MACRO_FUNCTION_HANDLE_COUNTER.fetch_add(1, AtomicOrdering::Relaxed) as i64,
    )
}

fn macroexpand_environment_handle_symbol() -> BlissVal {
    resolve_sym("BLISS::MACROEXPAND-ENV").unwrap_or(NIL)
}

fn store_macroexpand_environment(env: MacroexpandEnv) -> BlissVal {
    let id = NEXT_MACROEXPAND_ENVIRONMENT_ID.with(|counter| {
        let id = *counter.borrow();
        *counter.borrow_mut() = id + 1;
        id
    });
    MACROEXPAND_ENVIRONMENTS.with(|envs| {
        envs.borrow_mut().insert(id, env);
    });
    arena_cons(
        macroexpand_environment_handle_symbol(),
        BlissVal::from_fixnum(id as i64),
    )
}

fn load_macroexpand_environment(handle: BlissVal) -> Option<MacroexpandEnv> {
    if !handle.is_cons() {
        return None;
    }
    let (tag, payload) = cp(handle);
    if tag != macroexpand_environment_handle_symbol() || !payload.is_fixnum() {
        return None;
    }
    let id = payload.as_fixnum();
    if id <= 0 {
        return None;
    }
    MACROEXPAND_ENVIRONMENTS.with(|envs| envs.borrow().get(&(id as u64)).cloned())
}

fn store_control_value(token: &str, value: BlissVal) {
    CONTROL_VALUES.with(|values| {
        values.borrow_mut().insert(token.to_string(), value);
    });
}

fn freeze_env_frame(frame: &Rc<RefCell<EnvFrame>>) -> Arc<FrozenEnvFrame> {
    let borrowed = frame.borrow();
    Arc::new(FrozenEnvFrame {
        vars: borrowed.vars.clone(),
        symbol_vars: borrowed.symbol_vars.clone(),
        parent: borrowed.parent.as_ref().map(freeze_env_frame),
    })
}

fn thaw_env_frame(frame: &Arc<FrozenEnvFrame>) -> Rc<RefCell<EnvFrame>> {
    Rc::new(RefCell::new(EnvFrame {
        vars: frame.vars.clone(),
        symbol_vars: frame.symbol_vars.clone(),
        parent: frame.parent.as_ref().map(thaw_env_frame),
    }))
}

fn take_control_value(token: &str) -> BlissVal {
    CONTROL_VALUES.with(|values| values.borrow_mut().remove(token).unwrap_or(NIL))
}

fn handler_case_token(error: &BlissError) -> Option<String> {
    let BlissError::Internal(message) = error else {
        return None;
    };
    message
        .strip_prefix("__HANDLER_CASE__:")
        .map(ToString::to_string)
}

fn restart_invoked_name(error: &BlissError) -> Option<String> {
    let BlissError::Internal(message) = error else {
        return None;
    };
    message
        .strip_prefix("__RESTART_INVOKED__:")
        .map(ToString::to_string)
}

fn make_simple_error_condition(message: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let class = ensure_condition_class_registered(env, "SIMPLE-ERROR")?;
    bliss_stdlib::make_instance(
        class,
        &[
            resolve_sym("FORMAT-CONTROL").unwrap_or(NIL),
            message,
            resolve_sym("FORMAT-ARGUMENTS").unwrap_or(NIL),
            NIL,
        ],
    )
}

fn eval_handler_impl(
    handler: &HandlerImpl,
    condition: BlissVal,
    env: &mut Env,
) -> Result<(), BlissError> {
    match handler {
        HandlerImpl::Function(function) => {
            let _ = apply_function(*function, &[condition], env)?;
            Ok(())
        }
        HandlerImpl::HandlerCase {
            token,
            var_name: _,
            body: _,
            captured_frame: _,
        } => {
            store_control_value(token, condition);
            Err(BlissError::Internal(format!("__HANDLER_CASE__:{token}")))
        }
    }
}

fn signal_condition_object(condition: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    maybe_break_on_signals(condition, env)?;
    let handlers = env.handlers.clone();
    for handler in handlers.iter().rev() {
        if condition_matches_handler(env, condition, &handler.type_name) {
            eval_handler_impl(&handler.handler, condition, env)?;
        }
    }
    Ok(NIL)
}

fn condition_matches_type_spec(env: &Env, condition: BlissVal, type_spec: BlissVal) -> bool {
    if type_spec.is_nil() {
        return false;
    }
    if type_spec == T {
        return true;
    }
    if type_spec.is_symbol() {
        return condition_matches_handler(env, condition, &sym_name(type_spec));
    }
    let Ok(class) = resolve_class_metaobject(env, type_spec) else {
        return false;
    };
    bliss_stdlib::compute_class_precedence_list(bliss_stdlib::class_of(condition))
        .map(|cpl| cpl.into_iter().any(|entry| entry == class))
        .unwrap_or(false)
}

fn maybe_break_on_signals(condition: BlissVal, env: &mut Env) -> Result<(), BlissError> {
    let Some(type_spec) = env.lookup_var("*BREAK-ON-SIGNALS*") else {
        return Ok(());
    };
    if !condition_matches_type_spec(env, condition, type_spec) {
        return Ok(());
    }

    env.set_var("*BREAK-ON-SIGNALS*", NIL);
    let debugger_result = bliss_stdlib::invoke_debugger(condition);
    env.set_var("*BREAK-ON-SIGNALS*", type_spec);
    match debugger_result {
        Ok(()) | Err(_) => Ok(()),
    }
}

fn method_combination_from_name(name: &str) -> Option<bliss_stdlib::MethodCombinationType> {
    match symbol_bare_name(name).as_str() {
        "STANDARD" => Some(bliss_stdlib::MethodCombinationType::Standard),
        "+" | "PLUS" => Some(bliss_stdlib::MethodCombinationType::Plus),
        "AND" => Some(bliss_stdlib::MethodCombinationType::And),
        "OR" => Some(bliss_stdlib::MethodCombinationType::Or),
        "LIST" => Some(bliss_stdlib::MethodCombinationType::List),
        "APPEND" => Some(bliss_stdlib::MethodCombinationType::Append),
        "NCONC" => Some(bliss_stdlib::MethodCombinationType::Nconc),
        "MIN" => Some(bliss_stdlib::MethodCombinationType::Min),
        "MAX" => Some(bliss_stdlib::MethodCombinationType::Max),
        "PROGN" => Some(bliss_stdlib::MethodCombinationType::Progn),
        _ => None,
    }
}

fn resolve_class_metaobject(env: &Env, class: BlissVal) -> Result<BlissVal, BlissError> {
    if !class.is_symbol() {
        return Ok(class);
    }
    if let Some(found) = bliss_stdlib::find_class(class) {
        return Ok(found);
    }
    let name = sym_name(class);
    if builtin_condition_definition(&name).is_some() {
        return ensure_condition_class_registered(env, &name);
    }
    Ok(class)
}

fn resolve_slot_symbol(
    class_name: &str,
    key: BlissVal,
    env: &Env,
) -> Result<BlissVal, BlissError> {
    let key_name = symbol_bare_name(&sym_name(key));
    if let Some(slot) = lookup_slot_by_initarg(env, class_name, &key_name) {
        return Ok(resolve_sym(&slot.name).unwrap_or(NIL));
    }
    Err(BlissError::Internal(format!(
        "Unknown initarg :{} for class {}",
        key_name, class_name
    )))
}

fn class_name_for_instance_class(class: BlissVal) -> String {
    if class.is_symbol() {
        return sym_name(class);
    }
    let name = bliss_stdlib::class_name(class);
    if name.is_symbol() {
        sym_name(name)
    } else {
        val_as_str(class)
    }
}

fn lookup_slot_def<'a>(env: &'a Env, class_name: &str, slot_name: &str) -> Option<&'a SlotDef> {
    let class_def = env.classes.get(class_name)?;
    if let Some(slot) = class_def.slots.iter().find(|slot| slot.name == slot_name) {
        return Some(slot);
    }
    for super_name in &class_def.supers {
        if let Some(slot) = lookup_slot_def(env, super_name, slot_name) {
            return Some(slot);
        }
    }
    None
}

fn lookup_slot_by_initarg<'a>(env: &'a Env, class_name: &str, initarg: &str) -> Option<&'a SlotDef> {
    let class_def = env.classes.get(class_name)?;
    if let Some(slot) = class_def.slots.iter().find(|slot| {
        slot.initarg
            .as_ref()
            .map(|slot_initarg| slot_initarg == initarg)
            .unwrap_or(false)
            || slot.name == initarg
    }) {
        return Some(slot);
    }
    for super_name in &class_def.supers {
        if let Some(slot) = lookup_slot_by_initarg(env, super_name, initarg) {
            return Some(slot);
        }
    }
    None
}

fn split_initargs_for_class(
    env: &Env,
    class_name: &str,
    initargs: &[BlissVal],
) -> (Vec<BlissVal>, Vec<(String, BlissVal)>) {
    let mut instance_initargs = Vec::new();
    let mut class_initargs = Vec::new();
    let mut i = 0;
    while i + 1 < initargs.len() {
        let slot_sym = initargs[i];
        let value = initargs[i + 1];
        let slot_name = symbol_bare_name(&sym_name(slot_sym));
        if matches!(
            lookup_slot_def(env, class_name, &slot_name).map(|slot| slot.allocation),
            Some(SlotAllocation::Class)
        ) {
            class_initargs.push((slot_name, value));
        } else {
            instance_initargs.push(slot_sym);
            instance_initargs.push(value);
        }
        i += 2;
    }
    (instance_initargs, class_initargs)
}

fn write_class_slot_value(env: &Env, class_name: &str, slot_name: &str, value: Option<BlissVal>) {
    if let Some(class_def) = env.classes.get(class_name) {
        class_def
            .class_slot_values
            .lock()
            .unwrap()
            .insert(slot_name.to_string(), value);
    }
}

fn read_slot_value(instance: BlissVal, slot: BlissVal, env: &Env) -> Result<BlissVal, BlissError> {
    let class_name = class_name_for_instance_class(bliss_stdlib::class_of(instance));
    let slot_name = symbol_bare_name(&sym_name(slot));
    if matches!(
        lookup_slot_def(env, &class_name, &slot_name).map(|slot| slot.allocation),
        Some(SlotAllocation::Class)
    ) {
        if let Some(class_def) = env.classes.get(&class_name) {
            if let Some(Some(value)) = class_def.class_slot_values.lock().unwrap().get(&slot_name) {
                return Ok(*value);
            }
        }
        return Err(BlissError::UnboundVariable(slot));
    }
    bliss_stdlib::slot_value(instance, slot)
}

fn slot_is_bound(instance: BlissVal, slot: BlissVal, env: &Env) -> Result<bool, BlissError> {
    let class_name = class_name_for_instance_class(bliss_stdlib::class_of(instance));
    let slot_name = symbol_bare_name(&sym_name(slot));
    if matches!(
        lookup_slot_def(env, &class_name, &slot_name).map(|slot| slot.allocation),
        Some(SlotAllocation::Class)
    ) {
        if let Some(class_def) = env.classes.get(&class_name) {
            return Ok(matches!(
                class_def.class_slot_values.lock().unwrap().get(&slot_name),
                Some(Some(_))
            ));
        }
    }
    bliss_stdlib::slot_boundp(instance, slot)
}

fn write_slot_value(
    instance: BlissVal,
    slot: BlissVal,
    value: BlissVal,
    env: &Env,
) -> Result<(), BlissError> {
    let class_name = class_name_for_instance_class(bliss_stdlib::class_of(instance));
    let slot_name = symbol_bare_name(&sym_name(slot));
    if matches!(
        lookup_slot_def(env, &class_name, &slot_name).map(|slot| slot.allocation),
        Some(SlotAllocation::Class)
    ) {
        write_class_slot_value(env, &class_name, &slot_name, Some(value));
        return Ok(());
    }
    bliss_stdlib::set_slot_value(instance, slot, value)
}

fn apply_class_initforms(
    instance: BlissVal,
    class_name: &str,
    env: &mut Env,
    eligible_slots: Option<&[String]>,
    explicit_slots: &[String],
) -> Result<(), BlissError> {
    let Some(class_def) = env.classes.get(class_name).cloned() else {
        return Ok(());
    };
    for slot in &class_def.slots {
        if explicit_slots.iter().any(|name| name == &slot.name) {
            continue;
        }
        if let Some(eligible) = eligible_slots
            && !eligible.iter().any(|name| name == &slot.name)
        {
            continue;
        }
        let Some(initform) = slot.initform else {
            continue;
        };
        let slot_sym = resolve_sym(&slot.name).unwrap_or(NIL);
        let already_bound = match slot.allocation {
            SlotAllocation::Class => matches!(
                class_def.class_slot_values.lock().unwrap().get(&slot.name),
                Some(Some(_))
            ),
            SlotAllocation::Instance => bliss_stdlib::slot_boundp(instance, slot_sym)?,
        };
        if already_bound {
            continue;
        }
        let value = eval_form(initform, env)?;
        match slot.allocation {
            SlotAllocation::Class => {
                class_def
                    .class_slot_values
                    .lock()
                    .unwrap()
                    .insert(slot.name.clone(), Some(value));
            }
            SlotAllocation::Instance => {
                bliss_stdlib::set_slot_value(instance, slot_sym, value)?;
            }
        }
    }
    Ok(())
}

fn evaluated_initargs(
    class_name: &str,
    init_args: BlissVal,
    env: &mut Env,
) -> Result<Vec<BlissVal>, BlissError> {
    let args_vec = list_to_vec(init_args);
    let mut initargs = Vec::new();
    let mut i = 0;
    while i + 1 < args_vec.len() {
        let key = eval_form(args_vec[i], env)?;
        let value = eval_form(args_vec[i + 1], env)?;
        initargs.push(resolve_slot_symbol(class_name, key, env)?);
        initargs.push(value);
        i += 2;
    }
    Ok(initargs)
}

fn method_specificity_vector(
    env: &Env,
    method: &MethodDef,
    args: &[BlissVal],
) -> Option<Vec<usize>> {
    if method.specializers.len() > args.len() {
        return None;
    }
    let mut distances = Vec::with_capacity(method.specializers.len());
    for (arg, specializer) in args.iter().zip(method.specializers.iter()) {
        match specializer {
            MethodSpecializer::Any => distances.push(usize::MAX / 4),
            MethodSpecializer::Eql(expected) => {
                if arg != expected {
                    return None;
                }
                distances.push(0);
            }
            MethodSpecializer::Class(name) => {
                let specializer_sym = resolve_sym(name).unwrap_or(NIL);
                let specializer_class = resolve_class_metaobject(env, specializer_sym).ok()?;
                let arg_class = bliss_stdlib::class_of(*arg);
                let cpl = bliss_stdlib::compute_class_precedence_list(arg_class).ok()?;
                let pos = cpl.iter().position(|&class| class == specializer_class)?;
                distances.push(pos + 1);
            }
        }
    }
    Some(distances)
}

fn bind_method_params(
    env: &mut Env,
    method: &MethodDef,
    args: &[BlissVal],
) -> Result<(), BlissError> {
    for (i, param) in method.params.iter().enumerate() {
        env.define_local(param, args.get(i).copied().unwrap_or(NIL));
    }
    Ok(())
}

fn invoke_method(
    env: &mut Env,
    method: &MethodDef,
    args: &[BlissVal],
    next: Option<NextMethod>,
) -> Result<BlissVal, BlissError> {
    let mut child_env = env.child();
    bind_method_params(&mut child_env, method, args)?;
    if let Some(next) = next {
        child_env.method_context.push(MethodContext {
            args: args.to_vec(),
            next,
        });
    }
    eval_progn(method.body, &mut child_env)
}

fn invoke_restart_function(
    function: &RestartFunction,
    args: &[BlissVal],
    env: &mut Env,
) -> Result<BlissVal, BlissError> {
    match function {
        RestartFunction::FunctionForm {
            function_form,
            captured_frame,
        } => {
            let mut restart_env = env.child_with_parent(thaw_env_frame(captured_frame));
            let function = eval_form(*function_form, &mut restart_env)?;
            apply_function(function, args, &mut restart_env)
        }
        RestartFunction::ContinueNil => Ok(args.first().copied().unwrap_or(NIL)),
    }
}

fn restart_applies(
    restart: &RestartEntry,
    condition: Option<BlissVal>,
    env: &mut Env,
) -> Result<bool, BlissError> {
    let Some(condition) = condition else {
        return Ok(true);
    };
    let Some(test_function) = &restart.test_function else {
        return Ok(true);
    };
    Ok(!invoke_restart_function(test_function, &[condition], env)?.is_nil())
}

fn invoke_primary_chain(
    env: &mut Env,
    primary: &[MethodDef],
    args: &[BlissVal],
) -> Result<BlissVal, BlissError> {
    let Some((method, rest)) = primary.split_first() else {
        return Err(BlissError::Internal(
            "no next method available for CALL-NEXT-METHOD".into(),
        ));
    };
    invoke_method(
        env,
        method,
        args,
        Some(NextMethod::Primary {
            primary: rest.to_vec(),
        }),
    )
}

fn invoke_standard_methods(
    env: &mut Env,
    around: &[MethodDef],
    before: &[MethodDef],
    primary: &[MethodDef],
    after: &[MethodDef],
    args: &[BlissVal],
) -> Result<BlissVal, BlissError> {
    if let Some((method, rest)) = around.split_first() {
        return invoke_method(
            env,
            method,
            args,
            Some(NextMethod::Standard {
                around: rest.to_vec(),
                before: before.to_vec(),
                primary: primary.to_vec(),
                after: after.to_vec(),
            }),
        );
    }

    for method in before {
        let _ = invoke_method(env, method, args, None)?;
    }
    let result = invoke_primary_chain(env, primary, args)?;
    for method in after {
        let _ = invoke_method(env, method, args, None)?;
    }
    Ok(result)
}

fn invoke_next_method(
    env: &mut Env,
    context: &MethodContext,
    args: &[BlissVal],
) -> Result<BlissVal, BlissError> {
    match &context.next {
        NextMethod::Standard {
            around,
            before,
            primary,
            after,
        } => invoke_standard_methods(env, around, before, primary, after, args),
        NextMethod::Primary { primary } => invoke_primary_chain(env, primary, args),
    }
}

fn combine_short_form_results(
    combination: bliss_stdlib::MethodCombinationType,
    results: &[BlissVal],
) -> Result<BlissVal, BlissError> {
    match combination {
        bliss_stdlib::MethodCombinationType::List => Ok(vec_to_list(results)),
        bliss_stdlib::MethodCombinationType::Append
        | bliss_stdlib::MethodCombinationType::Nconc => {
            let mut combined = Vec::new();
            for result in results {
                combined.extend(list_to_vec(*result));
            }
            Ok(vec_to_list(&combined))
        }
        bliss_stdlib::MethodCombinationType::Progn
        | bliss_stdlib::MethodCombinationType::Standard => {
            Ok(results.last().copied().unwrap_or(NIL))
        }
        bliss_stdlib::MethodCombinationType::Plus => {
            let mut sum = 0.0;
            let mut is_float = false;
            for result in results {
                sum += num_val(*result)?;
                is_float |= result.is_single_float();
            }
            Ok(if is_float {
                BlissVal::from_single_float(sum as f32)
            } else {
                BlissVal::from_fixnum(sum as i64)
            })
        }
        bliss_stdlib::MethodCombinationType::And => {
            let mut last = T;
            for result in results {
                if result.is_nil() {
                    return Ok(NIL);
                }
                last = *result;
            }
            Ok(last)
        }
        bliss_stdlib::MethodCombinationType::Or => {
            for result in results {
                if !result.is_nil() {
                    return Ok(*result);
                }
            }
            Ok(NIL)
        }
        bliss_stdlib::MethodCombinationType::Min => {
            let mut best = *results.first().unwrap_or(&NIL);
            let mut best_num = num_val(best)?;
            for result in &results[1..] {
                let num = num_val(*result)?;
                if num < best_num {
                    best = *result;
                    best_num = num;
                }
            }
            Ok(best)
        }
        bliss_stdlib::MethodCombinationType::Max => {
            let mut best = *results.first().unwrap_or(&NIL);
            let mut best_num = num_val(best)?;
            for result in &results[1..] {
                let num = num_val(*result)?;
                if num > best_num {
                    best = *result;
                    best_num = num;
                }
            }
            Ok(best)
        }
    }
}

fn invoke_generic_function(
    name: &str,
    args: &[BlissVal],
    env: &mut Env,
) -> Result<BlissVal, BlissError> {
    let methods = env.methods.get(name).cloned().unwrap_or_default();
    if methods.is_empty() {
        return Err(BlissError::Internal(format!(
            "No applicable method for {}",
            name
        )));
    }

    let mut applicable: Vec<(MethodDef, Vec<usize>)> = methods
        .into_iter()
        .filter_map(|method| method_specificity_vector(env, &method, args).map(|key| (method, key)))
        .collect();
    applicable.sort_by(|a, b| a.1.cmp(&b.1));
    if applicable.is_empty() {
        return Err(BlissError::Internal(format!(
            "No applicable method for {}",
            name
        )));
    }

    let ordered: Vec<MethodDef> = applicable.into_iter().map(|(method, _)| method).collect();
    let mut method_map = HashMap::new();
    let method_ids: Vec<BlissVal> = ordered
        .iter()
        .map(|method| {
            method_map.insert(method.method_id, method.clone());
            method.method_id
        })
        .collect();

    let combination = env
        .generics
        .get(name)
        .map(|generic| generic.combination)
        .unwrap_or(bliss_stdlib::MethodCombinationType::Standard);
    let effective = bliss_stdlib::compute_effective_method(NIL, combination, &method_ids)?;
    match combination {
        bliss_stdlib::MethodCombinationType::Standard => {
            let Some((around_ids, before_ids, primary_ids, after_ids)) =
                bliss_stdlib::clos::get_effective_method(effective)
            else {
                return Err(BlissError::Internal(
                    "missing standard effective method".into(),
                ));
            };
            let around: Vec<MethodDef> = around_ids
                .into_iter()
                .filter_map(|id| method_map.get(&id).cloned())
                .collect();
            let before: Vec<MethodDef> = before_ids
                .into_iter()
                .filter_map(|id| method_map.get(&id).cloned())
                .collect();
            let primary: Vec<MethodDef> = primary_ids
                .into_iter()
                .filter_map(|id| method_map.get(&id).cloned())
                .collect();
            let after: Vec<MethodDef> = after_ids
                .into_iter()
                .filter_map(|id| method_map.get(&id).cloned())
                .collect();
            invoke_standard_methods(env, &around, &before, &primary, &after, args)
        }
        other => {
            let Some((short_combination, method_ids)) =
                bliss_stdlib::clos::get_short_form_method(effective)
            else {
                return Err(BlissError::Internal(
                    "missing short-form effective method".into(),
                ));
            };
            debug_assert_eq!(other, short_combination);
            let mut results = Vec::new();
            for method_id in method_ids {
                let method = method_map.get(&method_id).ok_or_else(|| {
                    BlissError::Internal("short-form method lookup failed".into())
                })?;
                results.push(invoke_method(env, method, args, None)?);
            }
            combine_short_form_results(short_combination, &results)
        }
    }
}

impl Env {
    fn new(sandbox: bool) -> Self {
        let _ = bliss_stdlib::bootstrap_clos();
        let mut packages = HashMap::new();
        seed_standard_packages(&mut packages);
        let mut env = Env {
            frame: Rc::new(RefCell::new(EnvFrame::default())),
            funs: Rc::new(HashMap::new()),
            macros: Rc::new(HashMap::new()),
            symbol_macros: Rc::new(HashMap::new()),
            classes: Rc::new(HashMap::new()),
            generics: Rc::new(HashMap::new()),
            methods: Rc::new(HashMap::new()),
            packages: Rc::new(packages),
            current_package: "COMMON-LISP-USER".to_string(),
            sandbox,
            restarts: Vec::new(),
            handlers: Vec::new(),
            mv: Vec::new(),
            mv_active: false,
            closures: Rc::new(RefCell::new(HashMap::new())),
            block_stack: Vec::new(),
            catch_stack: Vec::new(),
            tag_stack: Vec::new(),
            method_context: Vec::new(),
            eval_context: EvalContext::Repl,
        };
        env.define_local("*MODULE-PROVIDER-FUNCTIONS*", NIL);
        env.define_local("*LOAD-HOOKS*", NIL);
        env.define_local(
            "*FEATURES*",
            vec_to_list(&[resolve_sym(":BLISS").unwrap_or(NIL)]),
        );
        env.define_local("*PACKAGE*", arena_str("COMMON-LISP-USER"));
        env.define_local("*TYPE-DEFINITIONS*", NIL);
        env.define_local("*CONDITION-TYPES*", NIL);
        env.define_local("*CONDITION-DEFINITIONS*", NIL);
        env.define_local("*BREAK-ON-SIGNALS*", NIL);
        env
    }

    /// Create a child environment that shares global definitions (funs, macros,
    /// classes, methods, packages) via Rc and only clones local vars.
    fn child(&self) -> Self {
        Env {
            frame: Rc::new(RefCell::new(EnvFrame {
                vars: HashMap::new(),
                symbol_vars: HashMap::new(),
                parent: Some(Rc::clone(&self.frame)),
            })),
            funs: Rc::clone(&self.funs),
            macros: Rc::clone(&self.macros),
            symbol_macros: Rc::clone(&self.symbol_macros),
            classes: Rc::clone(&self.classes),
            generics: Rc::clone(&self.generics),
            methods: Rc::clone(&self.methods),
            packages: Rc::clone(&self.packages),
            current_package: self.current_package.clone(),
            sandbox: self.sandbox,
            restarts: self.restarts.clone(),
            handlers: self.handlers.clone(),
            mv: self.mv.clone(),
            mv_active: self.mv_active,
            closures: Rc::clone(&self.closures),
            block_stack: self.block_stack.clone(),
            catch_stack: self.catch_stack.clone(),
            tag_stack: self.tag_stack.clone(),
            method_context: self.method_context.clone(),
            eval_context: self.eval_context,
        }
    }

    fn child_with_parent(&self, parent: Rc<RefCell<EnvFrame>>) -> Self {
        Env {
            frame: Rc::new(RefCell::new(EnvFrame {
                vars: HashMap::new(),
                symbol_vars: HashMap::new(),
                parent: Some(parent),
            })),
            funs: Rc::clone(&self.funs),
            macros: Rc::clone(&self.macros),
            symbol_macros: Rc::clone(&self.symbol_macros),
            classes: Rc::clone(&self.classes),
            generics: Rc::clone(&self.generics),
            methods: Rc::clone(&self.methods),
            packages: Rc::clone(&self.packages),
            current_package: self.current_package.clone(),
            sandbox: self.sandbox,
            restarts: self.restarts.clone(),
            handlers: self.handlers.clone(),
            mv: self.mv.clone(),
            mv_active: self.mv_active,
            closures: Rc::clone(&self.closures),
            block_stack: self.block_stack.clone(),
            catch_stack: self.catch_stack.clone(),
            tag_stack: self.tag_stack.clone(),
            method_context: self.method_context.clone(),
            eval_context: self.eval_context,
        }
    }

    fn lookup_var(&self, name: &str) -> Option<BlissVal> {
        let frame = self.frame.borrow();
        if let Some(val) = frame.vars.get(name) {
            return Some(*val);
        }
        let parent = frame.parent.clone();
        drop(frame);
        parent.and_then(|parent| Self::lookup_frame(&parent, name))
    }

    fn lookup_var_symbol(&self, symbol: BlissVal) -> Option<BlissVal> {
        let frame = self.frame.borrow();
        if let Some(val) = frame.symbol_vars.get(&symbol.as_symbol_index()) {
            return Some(*val);
        }
        let parent = frame.parent.clone();
        drop(frame);
        parent.and_then(|parent| Self::lookup_symbol_frame(&parent, symbol.as_symbol_index()))
    }

    fn set_var(&mut self, name: &str, val: BlissVal) {
        if Self::set_frame_var(&self.frame, name, val) {
            return;
        }
        self.define_local(name, val);
    }

    fn set_var_symbol(&mut self, symbol: BlissVal, val: BlissVal) {
        if Self::set_symbol_frame_var(&self.frame, symbol.as_symbol_index(), val) {
            return;
        }
        let name = sym_name(symbol);
        if Self::set_frame_var(&self.frame, &name, val) {
            return;
        }
        self.define_local_symbol(symbol, val);
    }

    fn lookup_symbol_macro(&self, symbol: BlissVal) -> Option<BlissVal> {
        self.symbol_macros.get(&symbol.as_symbol_index()).copied()
    }

    fn define_symbol_macro(&mut self, symbol: BlissVal, expansion: BlissVal) {
        Rc::make_mut(&mut self.symbol_macros).insert(symbol.as_symbol_index(), expansion);
    }

    fn define_local(&mut self, name: &str, val: BlissVal) {
        self.frame.borrow_mut().vars.insert(name.to_string(), val);
    }

    fn define_local_symbol(&mut self, symbol: BlissVal, val: BlissVal) {
        let mut frame = self.frame.borrow_mut();
        frame.symbol_vars.insert(symbol.as_symbol_index(), val);
        frame.vars.insert(sym_name(symbol), val);
    }

    fn lookup_frame(frame: &Rc<RefCell<EnvFrame>>, name: &str) -> Option<BlissVal> {
        let borrowed = frame.borrow();
        if let Some(val) = borrowed.vars.get(name) {
            return Some(*val);
        }
        let parent = borrowed.parent.clone();
        drop(borrowed);
        parent.and_then(|parent| Self::lookup_frame(&parent, name))
    }

    fn lookup_symbol_frame(frame: &Rc<RefCell<EnvFrame>>, symbol_index: u32) -> Option<BlissVal> {
        let borrowed = frame.borrow();
        if let Some(val) = borrowed.symbol_vars.get(&symbol_index) {
            return Some(*val);
        }
        let parent = borrowed.parent.clone();
        drop(borrowed);
        parent.and_then(|parent| Self::lookup_symbol_frame(&parent, symbol_index))
    }

    fn set_frame_var(frame: &Rc<RefCell<EnvFrame>>, name: &str, val: BlissVal) -> bool {
        {
            let mut borrowed = frame.borrow_mut();
            if borrowed.vars.contains_key(name) {
                borrowed.vars.insert(name.to_string(), val);
                return true;
            }
            let parent = borrowed.parent.clone();
            drop(borrowed);
            if let Some(parent) = parent {
                return Self::set_frame_var(&parent, name, val);
            }
        }
        false
    }

    fn set_symbol_frame_var(
        frame: &Rc<RefCell<EnvFrame>>,
        symbol_index: u32,
        val: BlissVal,
    ) -> bool {
        {
            let mut borrowed = frame.borrow_mut();
            if let std::collections::hash_map::Entry::Occupied(mut entry) =
                borrowed.symbol_vars.entry(symbol_index)
            {
                entry.insert(val);
                return true;
            }
            let parent = borrowed.parent.clone();
            drop(borrowed);
            if let Some(parent) = parent {
                return Self::set_symbol_frame_var(&parent, symbol_index, val);
            }
        }
        false
    }

    fn clear_mv(&mut self) {
        self.mv.clear();
        self.mv_active = false;
    }

    fn set_mv(&mut self, values: Vec<BlissVal>) {
        self.mv = values;
        self.mv_active = true;
    }
}

fn with_child_frame<T>(
    env: &mut Env,
    parent: Rc<RefCell<EnvFrame>>,
    f: impl FnOnce(&mut Env) -> Result<T, BlissError>,
) -> Result<T, BlissError> {
    let saved_frame = Rc::clone(&env.frame);
    env.frame = Rc::new(RefCell::new(EnvFrame {
        vars: HashMap::new(),
        symbol_vars: HashMap::new(),
        parent: Some(parent),
    }));
    let result = f(env);
    env.frame = saved_frame;
    result
}

fn eval_lambda_call(
    env: &mut Env,
    params_form: BlissVal,
    body: BlissVal,
    args: &[BlissVal],
    parent: Rc<RefCell<EnvFrame>>,
) -> Result<BlissVal, BlissError> {
    with_child_frame(env, parent, |env| {
        bind_lambda_list(params_form, args, env)?;
        eval_progn(body, env)
    })
}

fn with_eval_context<T>(
    env: &mut Env,
    context: EvalContext,
    f: impl FnOnce(&mut Env) -> Result<T, BlissError>,
) -> Result<T, BlissError> {
    let previous = env.eval_context;
    env.eval_context = context;
    let result = f(env);
    env.eval_context = previous;
    result
}

fn seed_standard_packages(packages: &mut HashMap<String, PackageDef>) {
    for (name, uses) in [
        ("COMMON-LISP", Vec::<String>::new()),
        ("COMMON-LISP-USER", vec!["COMMON-LISP".to_string()]),
        ("KEYWORD", Vec::<String>::new()),
        ("BLISS-INTERNAL", Vec::<String>::new()),
        ("BLISS-EXT", vec!["COMMON-LISP".to_string()]),
    ] {
        packages.insert(
            name.to_string(),
            PackageDef {
                name: name.to_string(),
                exports: Vec::new(),
                uses,
                symbols: HashMap::new(),
            },
        );
        reader::register_package(name);
    }
    reader::register_package("CL-USER");
}

// ── BlissVal printer ──────────────────────────────────────────────
#[inline]
fn print_val(val: BlissVal, out: &mut String) {
    if val.is_nil() {
        out.push_str("NIL");
    } else if val == T {
        out.push('T');
    } else if val == EOF {
        out.push_str("#<EOF>");
    } else if val.is_fixnum() {
        out.push_str(&val.as_fixnum().to_string());
    } else if val.is_single_float() {
        let s = format!("{}", val.as_single_float());
        out.push_str(&s);
        if !s.contains('.') && !s.contains('e') {
            out.push_str(".0");
        }
    } else if val.is_character() {
        out.push_str("#\\");
        match val.as_char() {
            ' ' => out.push_str("Space"),
            '\n' => out.push_str("Newline"),
            '\t' => out.push_str("Tab"),
            '\r' => out.push_str("Return"),
            c => out.push(c),
        }
    } else if val.is_symbol() {
        let name = sym_name(val);
        if let Some(bare) = name.strip_prefix("KEYWORD:") {
            out.push(':');
            out.push_str(bare);
        } else {
            out.push_str(&name);
        }
    } else if val.is_cons() {
        out.push('(');
        print_list_body(val, out);
        out.push(')');
    } else if val.is_heap_object() {
        if let Some(s) = bliss_stdlib::registered_string(val) {
            out.push('"');
            for c in s.chars() {
                if c == '"' || c == '\\' {
                    out.push('\\');
                }
                out.push(c);
            }
            out.push('"');
            return;
        }
        unsafe {
            let ptr = val.as_ptr();
            let hdr = *(ptr as *const ObjectHeader);
            match hdr.type_id() {
                type_id::SIMPLE_BASE_STRING => {
                    let len = *(ptr.add(8) as *const u64) as usize;
                    let data = std::slice::from_raw_parts(ptr.add(16), len);
                    if let Ok(s) = std::str::from_utf8(data) {
                        out.push('"');
                        for c in s.chars() {
                            if c == '"' || c == '\\' {
                                out.push('\\');
                            }
                            out.push(c);
                        }
                        out.push('"');
                    } else {
                        out.push_str("#<string>");
                    }
                }
                type_id::SIMPLE_VECTOR => {
                    let len = *(ptr.add(8) as *const u64) as usize;
                    out.push_str("#(");
                    for i in 0..len {
                        if i > 0 {
                            out.push(' ');
                        }
                        print_val(*(ptr.add(16 + i * 8) as *const BlissVal), out);
                    }
                    out.push(')');
                }
                type_id::RATIO => {
                    let num = *(ptr.add(8) as *const BlissVal);
                    let den = *(ptr.add(16) as *const BlissVal);
                    print_val(num, out);
                    out.push('/');
                    print_val(den, out);
                }
                type_id::BIGNUM => {
                    let sign = *(ptr.add(8) as *const i32);
                    let n = *(ptr.add(12) as *const u32) as usize;
                    let mut limbs: Vec<u64> = Vec::with_capacity(n);
                    for i in 0..n {
                        limbs.push(*(ptr.add(16 + i * 8) as *const u64));
                    }
                    out.push_str(&bignum_to_decimal(sign, &limbs));
                }
                _ => out.push_str(&format!("#<heap-object type={}>", hdr.type_id())),
            }
        }
    } else {
        out.push_str(&format!("#<unknown {:#x}>", val.0));
    }
}

/// Render a bignum (little-endian base-2^64 limbs) as a decimal string.
fn bignum_to_decimal(sign: i32, limbs: &[u64]) -> String {
    if sign == 0 || limbs.iter().all(|&l| l == 0) {
        return "0".into();
    }
    const D: u128 = 1_000_000_000;
    let mut work = limbs.to_vec();
    let mut chunks: Vec<u32> = Vec::new();
    loop {
        let mut rem: u128 = 0;
        for limb in work.iter_mut().rev() {
            let cur = (rem << 64) | (*limb as u128);
            *limb = (cur / D) as u64;
            rem = cur % D;
        }
        chunks.push(rem as u32);
        while work.len() > 1 && *work.last().unwrap() == 0 {
            work.pop();
        }
        if work.len() == 1 && work[0] == 0 {
            break;
        }
    }
    let mut s = String::new();
    if sign < 0 {
        s.push('-');
    }
    for (i, chunk) in chunks.iter().rev().enumerate() {
        if i == 0 {
            s.push_str(&chunk.to_string());
        } else {
            s.push_str(&format!("{:09}", chunk));
        }
    }
    s
}

#[inline]
fn print_list_body(val: BlissVal, out: &mut String) {
    let mut cur = val;
    let mut first = true;
    while cur.is_cons() {
        if !first {
            out.push(' ');
        }
        first = false;
        unsafe {
            let c = cur.as_ptr() as *const ConsCell;
            print_val((*c).car, out);
            cur = (*c).cdr;
        }
    }
    if !cur.is_nil() {
        out.push_str(" . ");
        print_val(cur, out);
    }
}

#[inline]
fn format_val(val: BlissVal) -> String {
    let mut s = String::new();
    print_val(val, &mut s);
    s
}

fn simple_format_message(control: &str, args: &[BlissVal]) -> String {
    let mut rendered = String::new();
    let mut chars = control.chars().peekable();
    let mut arg_index = 0usize;
    while let Some(ch) = chars.next() {
        if ch == '~' {
            if let Some(directive) = chars.next() {
                match directive {
                    'A' | 'a' => {
                        if let Some(arg) = args.get(arg_index) {
                            let mut out = String::new();
                            princ_val(*arg, &mut out);
                            rendered.push_str(&out);
                            arg_index += 1;
                            continue;
                        }
                    }
                    'S' | 's' => {
                        if let Some(arg) = args.get(arg_index) {
                            rendered.push_str(&format_val(*arg));
                            arg_index += 1;
                            continue;
                        }
                    }
                    '~' => {
                        rendered.push('~');
                        continue;
                    }
                    _ => {
                        rendered.push('~');
                        rendered.push(directive);
                        continue;
                    }
                }
            }
        }
        rendered.push(ch);
    }
    rendered
}

#[inline]
fn princ_val(val: BlissVal, out: &mut String) {
    if val.is_heap_object() {
        unsafe {
            let ptr = val.as_ptr();
            let hdr = *(ptr as *const ObjectHeader);
            if hdr.type_id() == type_id::SIMPLE_BASE_STRING {
                let len = *(ptr.add(8) as *const u64) as usize;
                let data = std::slice::from_raw_parts(ptr.add(16), len);
                if let Ok(s) = std::str::from_utf8(data) {
                    out.push_str(s);
                    return;
                }
            }
        }
    }
    if val.is_character() {
        out.push(val.as_char());
        return;
    }
    if val.is_symbol() {
        let name = sym_name(val);
        // For ~A, print symbol name without package prefix
        let bare = name.trim_start_matches("KEYWORD:");
        out.push_str(bare);
        return;
    }
    print_val(val, out);
}

// ── Symbol name lookup ────────────────────────────────────────────
fn sym_name(val: BlissVal) -> String {
    if val.is_nil() {
        return "NIL".into();
    }
    if val == T {
        return "T".into();
    }
    if !val.is_symbol() {
        return String::new();
    }
    let idx = val.as_symbol_index();
    // Use the reader's reverse symbol table for O(1) lookup
    if let Some(name) = reader::symbol_name(idx) {
        return name;
    }
    format!("SYM#{}", idx)
}

fn resolve_sym(name: &str) -> Option<BlissVal> {
    match reader::read_from_string(name) {
        Ok((sym, _)) if sym.is_symbol() => Some(sym),
        _ => None,
    }
}

// ── Collect a list into a Vec of elements ─────────────────────────
fn list_to_vec(val: BlissVal) -> Vec<BlissVal> {
    let mut result = Vec::new();
    let mut c = val;
    while c.is_cons() {
        let (car, cdr) = cp(c);
        result.push(car);
        c = cdr;
    }
    result
}

fn vec_to_list(elems: &[BlissVal]) -> BlissVal {
    let mut result = NIL;
    for e in elems.iter().rev() {
        result = arena_cons(*e, result);
    }
    result
}

fn format_body_forms(forms: BlissVal) -> String {
    let parts = list_to_vec(forms)
        .into_iter()
        .map(format_val)
        .collect::<Vec<_>>();
    if parts.is_empty() {
        "NIL".to_string()
    } else {
        parts.join(" ")
    }
}

// ── Quasiquote expansion ─────────────────────────────────────────
/// Expand a quasiquote template, substituting BLISS::UNQUOTE forms with
/// their evaluated values and splicing BLISS::UNQUOTE-SPLICING forms.
fn eval_quasiquote(template: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    // If template is not a cons, return it as-is (like quote)
    if !template.is_cons() {
        return Ok(template);
    }
    let (car, cdr) = cp(template);

    // Check if template is (BLISS::UNQUOTE expr)
    if car.is_symbol() && sym_name(car) == "BLISS::UNQUOTE" {
        let (expr, _) = cp(cdr);
        return eval_form(expr, env);
    }

    // Check if template is (BLISS::UNQUOTE-SPLICING expr) at top level — error
    if car.is_symbol() && sym_name(car) == "BLISS::UNQUOTE-SPLICING" {
        return Err(BlissError::Internal(",@ not inside a list".into()));
    }

    // Recursively process each element of the list, handling splicing
    let mut result_elems: Vec<BlissVal> = Vec::new();
    let mut cur = template;
    while cur.is_cons() {
        let (elem, rest) = cp(cur);
        // Check if elem is (BLISS::UNQUOTE-SPLICING expr)
        if elem.is_cons() {
            let (ecar, ecdr) = cp(elem);
            if ecar.is_symbol() && sym_name(ecar) == "BLISS::UNQUOTE-SPLICING" {
                let (splice_expr, _) = cp(ecdr);
                let splice_val = eval_form(splice_expr, env)?;
                // Splice the list into the result
                let spliced = list_to_vec(splice_val);
                result_elems.extend(spliced);
                cur = rest;
                continue;
            }
        }
        // Regular element — recursively expand
        let expanded = eval_quasiquote(elem, env)?;
        result_elems.push(expanded);
        cur = rest;
    }
    // Handle dotted pair tail
    if !cur.is_nil() {
        let expanded_tail = eval_quasiquote(cur, env)?;
        // Build from the back with the non-nil tail
        let mut result = expanded_tail;
        for e in result_elems.iter().rev() {
            result = arena_cons(*e, result);
        }
        return Ok(result);
    }
    Ok(vec_to_list(&result_elems))
}

// ── Closure ID generation ────────────────────────────────────────
thread_local! {
    static NEXT_CLOSURE_ID: RefCell<u64> = const { RefCell::new(1) };
    static NEXT_STDLIB_CLASS_ID: RefCell<i64> = const { RefCell::new(300_000) };
}

fn next_closure_id() -> u64 {
    NEXT_CLOSURE_ID.with(|c| {
        let v = *c.borrow();
        *c.borrow_mut() = v + 1;
        v
    })
}

fn next_stdlib_class_id() -> BlissVal {
    NEXT_STDLIB_CLASS_ID.with(|c| {
        let v = *c.borrow();
        *c.borrow_mut() = v + 1;
        BlissVal::from_fixnum(v)
    })
}

fn register_declared_packages(source: &str) {
    let chars: Vec<char> = source.chars().collect();
    let mut pos = 0;
    while pos < chars.len() {
        match chars[pos] {
            ';' => {
                while pos < chars.len() && chars[pos] != '\n' {
                    pos += 1;
                }
            }
            '"' => {
                pos += 1;
                while pos < chars.len() {
                    if chars[pos] == '\\' {
                        pos += 2;
                    } else if chars[pos] == '"' {
                        pos += 1;
                        break;
                    } else {
                        pos += 1;
                    }
                }
            }
            '(' => {
                pos += 1;
                let op = read_scan_token(&chars, &mut pos);
                if op.eq_ignore_ascii_case("DEFPACKAGE") {
                    let pkg = read_scan_token(&chars, &mut pos);
                    let pkg = pkg
                        .trim_start_matches(':')
                        .trim_start_matches("KEYWORD:")
                        .trim();
                    if !pkg.is_empty() {
                        reader::register_package(&pkg.to_uppercase());
                    }
                }
            }
            _ => pos += 1,
        }
    }
}

fn read_scan_token(chars: &[char], pos: &mut usize) -> String {
    while *pos < chars.len() && chars[*pos].is_ascii_whitespace() {
        *pos += 1;
    }
    if *pos >= chars.len() {
        return String::new();
    }
    let start = *pos;
    while *pos < chars.len() {
        let ch = chars[*pos];
        if ch.is_ascii_whitespace() || matches!(ch, '(' | ')' | '"' | ';') {
            break;
        }
        *pos += 1;
    }
    chars[start..*pos].iter().collect()
}

// ── Minimal bootstrap evaluator ───────────────────────────────────
fn read_eval_all_env(source: &str, env: &mut Env) -> Result<BlissVal, BlissError> {
    register_declared_packages(source);
    let chars: Vec<char> = source.chars().collect();
    let mut pos = 0;
    let mut last = NIL;
    loop {
        while pos < chars.len() && chars[pos].is_ascii_whitespace() {
            pos += 1;
        }
        if pos >= chars.len() {
            break;
        }
        let remaining: String = chars[pos..].iter().collect();
        let (val, consumed) = reader::read_from_string(&remaining)?;
        if val == EOF {
            break;
        }
        last = eval_form(val, env)?;
        pos += consumed;
    }
    Ok(last)
}

fn bundled_asdf_path() -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../lib/asdf.lisp")
        .to_string_lossy()
        .into_owned()
}

/// The bootstrap prelude, embedded at compile time so it is always available
/// regardless of where the binary runs from.
const EMBEDDED_BOOT_LISP: &str = include_str!("../../../lib/boot.lisp");

/// Source of the bootstrap prelude: the file named by BLISS_BOOT_FILE if set
/// (for testing alternate preludes), otherwise the embedded copy.
fn boot_prelude_source() -> Result<String, BlissError> {
    if let Ok(p) = std::env::var("BLISS_BOOT_FILE") {
        return std::fs::read_to_string(&p).map_err(|e| {
            BlissError::FileError(format!("cannot read BLISS_BOOT_FILE {}: {}", p, e))
        });
    }
    Ok(EMBEDDED_BOOT_LISP.to_string())
}

// Reserved for the stage-6 ASDF output-translations wiring; retained so the
// default cache location stays defined in one place until `require`/ASDF is live.
#[allow(dead_code)]
fn default_asdf_output_translations() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    format!("{home}/.cache/bliss/asdf/")
}

fn normalize_package_name(name: &str) -> String {
    name.trim_start_matches("KEYWORD:")
        .trim_start_matches(':')
        .to_uppercase()
}

/// Short package name for the REPL prompt. Standard packages use their usual
/// CL nicknames (`CL-USER>`, `CL>`); user-defined packages show their full name.
fn prompt_package_name(name: &str) -> &str {
    match name {
        "COMMON-LISP-USER" => "CL-USER",
        "COMMON-LISP" => "CL",
        other => other,
    }
}

fn symbol_bare_name(name: &str) -> String {
    let without_keyword = name.trim_start_matches("KEYWORD:");
    let base = without_keyword
        .rsplit_once("::")
        .map(|(_, tail)| tail)
        .or_else(|| without_keyword.rsplit_once(':').map(|(_, tail)| tail))
        .unwrap_or(without_keyword);
    base.to_uppercase()
}

fn package_status_symbol(status: &str) -> BlissVal {
    resolve_sym(&format!(":{}", status)).unwrap_or(NIL)
}

fn symbol_for_package(pkg_name: &str, bare_name: &str) -> Option<BlissVal> {
    let pkg_name = normalize_package_name(pkg_name);
    let bare_name = bare_name.to_uppercase();
    let candidates = if pkg_name == "COMMON-LISP" || pkg_name == "COMMON-LISP-USER" {
        vec![bare_name.clone()]
    } else if pkg_name == "KEYWORD" {
        vec![format!(":{}", bare_name)]
    } else {
        vec![
            format!("{pkg_name}:{bare_name}"),
            format!("{pkg_name}::{bare_name}"),
            bare_name.clone(),
        ]
    };
    candidates
        .into_iter()
        .find_map(|candidate| resolve_sym(&candidate))
}

fn intern_into_package(env: &mut Env, pkg_name: &str, bare_name: &str) -> BlissVal {
    let pkg_name = normalize_package_name(pkg_name);
    let bare_name = bare_name.to_uppercase();
    let sym = symbol_for_package(&pkg_name, &bare_name)
        .or_else(|| resolve_sym(&bare_name))
        .unwrap_or_else(|| arena_str(&bare_name));
    ensure_package_available(env, &pkg_name, &[]);
    Rc::make_mut(&mut env.packages)
        .get_mut(&pkg_name)
        .expect("package exists")
        .symbols
        .insert(bare_name, sym);
    sym
}

fn find_symbol_in_package(
    env: &Env,
    pkg_name: &str,
    bare_name: &str,
) -> Option<(BlissVal, &'static str)> {
    let pkg_name = normalize_package_name(pkg_name);
    let bare_name = bare_name.to_uppercase();
    if pkg_name == "COMMON-LISP" || pkg_name == "COMMON-LISP-USER" {
        if let Some(sym) = resolve_sym(&bare_name) {
            return Some((sym, "EXTERNAL"));
        }
    }
    if pkg_name == "KEYWORD" {
        if let Some(sym) = resolve_sym(&format!(":{}", bare_name)) {
            return Some((sym, "EXTERNAL"));
        }
    }
    let package = env.packages.get(&pkg_name)?;
    if let Some(sym) = package.symbols.get(&bare_name) {
        let status = if package.exports.iter().any(|name| name == &bare_name) {
            "EXTERNAL"
        } else {
            "INTERNAL"
        };
        return Some((*sym, status));
    }
    for used in &package.uses {
        if let Some((sym, _)) = find_symbol_in_package(env, used, &bare_name) {
            return Some((sym, "INHERITED"));
        }
    }
    None
}

fn ensure_package_available(env: &mut Env, name: &str, uses: &[&str]) {
    let packages = Rc::make_mut(&mut env.packages);
    packages
        .entry(name.to_string())
        .or_insert_with(|| PackageDef {
            name: name.to_string(),
            exports: Vec::new(),
            uses: uses.iter().map(|pkg| (*pkg).to_string()).collect(),
            symbols: HashMap::new(),
        });
    reader::register_package(name);
}

fn plist_get(list: BlissVal, key: &str) -> Option<BlissVal> {
    let mut cur = list;
    while cur.is_cons() {
        let (entry, rest) = cp(cur);
        if entry.is_cons() {
            let (entry_key, entry_vals) = cp(entry);
            if symbol_bare_name(&val_as_str(entry_key)) == key {
                return Some(cp(entry_vals).0);
            }
        }
        cur = rest;
    }
    None
}

fn plist_entry(list: BlissVal, key: &str) -> Option<BlissVal> {
    let mut cur = list;
    while cur.is_cons() {
        let (entry, rest) = cp(cur);
        if entry.is_cons() {
            let (entry_key, _) = cp(entry);
            if symbol_bare_name(&val_as_str(entry_key)) == key {
                return Some(entry);
            }
        }
        cur = rest;
    }
    None
}

fn resolve_type_spec(env: &Env, type_spec: BlissVal) -> BlissVal {
    if type_spec.is_symbol() {
        let name = symbol_bare_name(&sym_name(type_spec));
        if let Some(expanded) =
            plist_get(env.lookup_var("*TYPE-DEFINITIONS*").unwrap_or(NIL), &name)
        {
            return expanded;
        }
    }
    type_spec
}

fn condition_definition_entry(env: &Env, type_name: &str) -> Option<BlissVal> {
    plist_entry(
        env.lookup_var("*CONDITION-DEFINITIONS*").unwrap_or(NIL),
        &symbol_bare_name(type_name),
    )
}

/// A condition definition: its parent type names and its `(slot, initarg)` slots.
type ConditionDefinition = (Vec<String>, Vec<(String, String)>);

fn builtin_condition_definition(type_name: &str) -> Option<ConditionDefinition> {
    match symbol_bare_name(type_name).as_str() {
        "CONDITION" => Some((vec![], vec![])),
        "SERIOUS-CONDITION" => Some((vec!["CONDITION".into()], vec![])),
        "ERROR" => Some((vec!["SERIOUS-CONDITION".into()], vec![])),
        "WARNING" => Some((vec!["CONDITION".into()], vec![])),
        "STYLE-WARNING" => Some((vec!["WARNING".into()], vec![])),
        "STORAGE-CONDITION" => Some((vec!["SERIOUS-CONDITION".into()], vec![])),
        "SIMPLE-CONDITION" => Some((
            vec!["CONDITION".into()],
            vec![
                ("FORMAT-CONTROL".into(), "FORMAT-CONTROL".into()),
                ("FORMAT-ARGUMENTS".into(), "FORMAT-ARGUMENTS".into()),
            ],
        )),
        "SIMPLE-ERROR" => Some((vec!["ERROR".into(), "SIMPLE-CONDITION".into()], vec![])),
        "SIMPLE-WARNING" => Some((vec!["WARNING".into(), "SIMPLE-CONDITION".into()], vec![])),
        "ARITHMETIC-ERROR" => Some((
            vec!["ERROR".into()],
            vec![
                ("OPERATION".into(), "OPERATION".into()),
                ("OPERANDS".into(), "OPERANDS".into()),
            ],
        )),
        "DIVISION-BY-ZERO" => Some((vec!["ARITHMETIC-ERROR".into()], vec![])),
        "FLOATING-POINT-OVERFLOW" => Some((vec!["ARITHMETIC-ERROR".into()], vec![])),
        "FLOATING-POINT-UNDERFLOW" => Some((vec!["ARITHMETIC-ERROR".into()], vec![])),
        "FLOATING-POINT-INEXACT" => Some((vec!["ARITHMETIC-ERROR".into()], vec![])),
        "FLOATING-POINT-INVALID-OPERATION" => {
            Some((vec!["ARITHMETIC-ERROR".into()], vec![]))
        }
        "CELL-ERROR" => Some((vec!["ERROR".into()], vec![("NAME".into(), "NAME".into())])),
        "UNBOUND-VARIABLE" => Some((vec!["CELL-ERROR".into()], vec![])),
        "UNDEFINED-FUNCTION" => Some((vec!["CELL-ERROR".into()], vec![])),
        "UNBOUND-SLOT" => Some((
            vec!["CELL-ERROR".into()],
            vec![("INSTANCE".into(), "INSTANCE".into())],
        )),
        "TYPE-ERROR" => Some((
            vec!["ERROR".into()],
            vec![
                ("DATUM".into(), "DATUM".into()),
                ("EXPECTED-TYPE".into(), "EXPECTED-TYPE".into()),
            ],
        )),
        "SIMPLE-TYPE-ERROR" => Some((
            vec!["TYPE-ERROR".into(), "SIMPLE-CONDITION".into()],
            vec![],
        )),
        "CONTROL-ERROR" => Some((vec!["ERROR".into()], vec![])),
        "FILE-ERROR" => Some((vec!["ERROR".into()], vec![("PATHNAME".into(), "PATHNAME".into())])),
        "PACKAGE-ERROR" => Some((vec!["ERROR".into()], vec![("PACKAGE".into(), "PACKAGE".into())])),
        "PARSE-ERROR" => Some((vec!["ERROR".into()], vec![])),
        "PRINT-NOT-READABLE" => Some((vec!["ERROR".into()], vec![("OBJECT".into(), "OBJECT".into())])),
        "PROGRAM-ERROR" => Some((vec!["ERROR".into()], vec![])),
        "STREAM-ERROR" => Some((vec!["ERROR".into()], vec![("STREAM".into(), "STREAM".into())])),
        "END-OF-FILE" => Some((vec!["STREAM-ERROR".into()], vec![])),
        "READER-ERROR" => Some((vec!["STREAM-ERROR".into(), "PARSE-ERROR".into()], vec![])),
        _ => None,
    }
}

fn condition_slot_specs(env: &Env, type_name: &str) -> Vec<(String, String)> {
    let mut specs = Vec::new();
    let type_name = symbol_bare_name(type_name);
    if let Some((parents, own_slots)) = builtin_condition_definition(&type_name) {
        for parent in parents {
            specs.extend(condition_slot_specs(env, &parent));
        }
        specs.extend(own_slots);
        return specs;
    }

    if let Some(entry) = condition_definition_entry(env, &type_name) {
        let (_, rest) = cp(entry);
        let (parents_form, rest2) = cp(rest);
        let (slots_form, _) = cp(rest2);
        for parent in list_to_vec(parents_form) {
            specs.extend(condition_slot_specs(env, &sym_name(parent)));
        }
        for slot in list_to_vec(slots_form) {
            if slot.is_symbol() {
                let name = symbol_bare_name(&sym_name(slot));
                specs.push((name.clone(), name));
                continue;
            }
            if slot.is_cons() {
                let (slot_name_form, opts_form) = cp(slot);
                let slot_name = symbol_bare_name(&sym_name(slot_name_form));
                let mut initarg = slot_name.clone();
                let opts = list_to_vec(opts_form);
                let mut i = 0;
                while i + 1 < opts.len() {
                    let opt_name = symbol_bare_name(&sym_name(opts[i]));
                    if opt_name == "INITARG" {
                        initarg = symbol_bare_name(&sym_name(opts[i + 1]));
                    }
                    i += 2;
                }
                specs.push((slot_name, initarg));
            }
        }
    }

    specs
}

fn condition_default_initargs(env: &Env, type_name: &str) -> Vec<(String, BlissVal)> {
    let mut defaults = Vec::new();
    let type_name = symbol_bare_name(type_name);
    if let Some(entry) = condition_definition_entry(env, &type_name) {
        let (_, rest) = cp(entry);
        let (parents_form, rest2) = cp(rest);
        let (_, rest3) = cp(rest2);
        let (options_form, _) = cp(rest3);
        for parent in list_to_vec(parents_form) {
            defaults.extend(condition_default_initargs(env, &sym_name(parent)));
        }
        for option in list_to_vec(options_form) {
            if !option.is_cons() {
                continue;
            }
            let (name_form, values_form) = cp(option);
            if symbol_bare_name(&sym_name(name_form)) != "DEFAULT-INITARGS" {
                continue;
            }
            let values = list_to_vec(values_form);
            let mut i = 0;
            while i + 1 < values.len() {
                defaults.push((symbol_bare_name(&sym_name(values[i])), values[i + 1]));
                i += 2;
            }
        }
    }
    defaults
}

fn ensure_condition_class_registered(env: &Env, type_name: &str) -> Result<BlissVal, BlissError> {
    let type_sym = resolve_sym(&symbol_bare_name(type_name)).unwrap_or(NIL);
    if let Some(class) = bliss_stdlib::find_class(type_sym) {
        return Ok(class);
    }

    let mut parent_names = Vec::new();
    if let Some((builtin_parents, _)) = builtin_condition_definition(type_name) {
        parent_names.extend(builtin_parents);
    } else if let Some(entry) = condition_definition_entry(env, type_name) {
        let (_, rest) = cp(entry);
        let (parents_form, _) = cp(rest);
        for parent in list_to_vec(parents_form) {
            parent_names.push(symbol_bare_name(&sym_name(parent)));
        }
    } else {
        parent_names.push("CONDITION".into());
    }

    let mut supers = Vec::new();
    for parent in parent_names {
        supers.push(ensure_condition_class_registered(env, &parent)?);
    }

    let slot_names: Vec<BlissVal> = condition_slot_specs(env, type_name)
        .into_iter()
        .map(|(slot_name, _)| resolve_sym(&slot_name).unwrap_or(NIL))
        .collect();
    let class = next_stdlib_class_id();
    bliss_stdlib::define_class(type_sym, class, &supers, &slot_names)?;
    Ok(class)
}

fn condition_type_hierarchy_names(cond: BlissVal) -> Option<Vec<String>> {
    let class = bliss_stdlib::class_of(cond);
    let cpl = bliss_stdlib::compute_class_precedence_list(class).ok()?;
    let mut names = Vec::new();
    for class in cpl {
        let name = bliss_stdlib::class_name(class);
        if name.is_symbol() {
            names.push(symbol_bare_name(&sym_name(name)));
        }
    }
    if names.iter().any(|name| name == "CONDITION") {
        Some(names)
    } else {
        None
    }
}

fn condition_supertypes(env: &Env, type_name: &str) -> Vec<String> {
    let mut supers = Vec::new();
    let mut cur = plist_get(
        env.lookup_var("*CONDITION-TYPES*").unwrap_or(NIL),
        &symbol_bare_name(type_name),
    );
    while let Some(list) = cur {
        for sup in list_to_vec(list) {
            supers.push(symbol_bare_name(&val_as_str(sup)));
        }
        cur = None;
    }
    supers
}

fn condition_type_matches(env: &Env, signaled_type: &str, handler_type: &str) -> bool {
    let signaled = symbol_bare_name(signaled_type);
    let handler = symbol_bare_name(handler_type);
    handler == "T"
        || signaled == handler
        || condition_supertypes(env, &signaled)
            .iter()
            .any(|sup| sup == &handler)
}

fn condition_matches_handler(env: &Env, condition: BlissVal, handler_type: &str) -> bool {
    let handler = symbol_bare_name(handler_type);
    if handler == "T" {
        return true;
    }
    if let Some(hierarchy) = condition_type_hierarchy_names(condition) {
        return hierarchy.iter().any(|name| name == &handler);
    }
    let signaled_type = if condition.is_symbol() {
        sym_name(condition)
    } else {
        val_as_str(condition)
    };
    condition_type_matches(env, &signaled_type, handler_type)
}

fn is_package_value(env: &Env, value: BlissVal) -> bool {
    if !value.is_string() {
        return false;
    }
    let pkg_name = normalize_package_name(&val_as_str(value));
    env.packages.contains_key(&pkg_name)
        || matches!(
            pkg_name.as_str(),
            "COMMON-LISP" | "COMMON-LISP-USER" | "KEYWORD" | "BLISS-EXT"
        )
}

fn typep_matches(env: &mut Env, object: BlissVal, type_spec: BlissVal) -> Result<bool, BlissError> {
    let type_spec = resolve_type_spec(env, type_spec);
    if type_spec.is_symbol() {
        let type_name = symbol_bare_name(&sym_name(type_spec));
        let matches = match type_name.as_str() {
            "T" => true,
            "NIL" | "NULL" => object.is_nil(),
            "ATOM" => !object.is_cons(),
            "LIST" => object.is_list(),
            "CONS" => object.is_cons(),
            "SYMBOL" => object.is_symbol(),
            "STRING" | "SIMPLE-STRING" | "BASE-STRING" => object.is_string(),
            "NUMBER" | "REAL" => object.is_fixnum() || object.is_single_float(),
            "INTEGER" | "FIXNUM" => object.is_fixnum(),
            "FLOAT" | "SINGLE-FLOAT" => object.is_single_float(),
            "CHARACTER" => object.is_character(),
            "BOOLEAN" => object.is_nil() || object == T,
            "FUNCTION" => object.is_symbol() || object.is_cons(),
            "PACKAGE" => is_package_value(env, object),
            "HASH-TABLE" => bliss_stdlib::hash_table_count(object).is_ok(),
            "PATHNAME" => bliss_stdlib::namestring(object).is_ok(),
            "STREAM" | "FILE-STREAM" | "SYNONYM-STREAM" => is_stream(object),
            other => {
                if let Some(hierarchy) = condition_type_hierarchy_names(object) {
                    hierarchy.iter().any(|name| name == other)
                } else {
                    other == symbol_bare_name(&val_as_str(object))
                }
            }
        };
        return Ok(matches);
    }

    if !type_spec.is_cons() {
        return Ok(object == type_spec);
    }

    let (head, args) = cp(type_spec);
    let op = symbol_bare_name(&sym_name(head));
    match op.as_str() {
        "OR" => {
            for spec in list_to_vec(args) {
                if typep_matches(env, object, spec)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        "AND" => {
            for spec in list_to_vec(args) {
                if !typep_matches(env, object, spec)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        "MEMBER" => Ok(list_to_vec(args)
            .into_iter()
            .any(|candidate| vals_equal(object, candidate))),
        "EQL" => {
            let (value, _) = cp(args);
            Ok(object == value)
        }
        "INTEGER" => {
            if !object.is_fixnum() {
                return Ok(false);
            }
            let bounds = list_to_vec(args);
            let value = object.as_fixnum();
            let lower_ok = bounds
                .first()
                .copied()
                .map(|bound| {
                    if bound.is_symbol() && symbol_bare_name(&sym_name(bound)) == "*" {
                        true
                    } else {
                        value >= bound.as_fixnum()
                    }
                })
                .unwrap_or(true);
            let upper_ok = bounds
                .get(1)
                .copied()
                .map(|bound| {
                    if bound.is_symbol() && symbol_bare_name(&sym_name(bound)) == "*" {
                        true
                    } else {
                        value <= bound.as_fixnum()
                    }
                })
                .unwrap_or(true);
            Ok(lower_ok && upper_ok)
        }
        "SATISFIES" => {
            let (predicate, _) = cp(args);
            let predicate_name = sym_name(predicate);
            if predicate_name == "FIND-PACKAGE" {
                let designator =
                    if object.is_character() || object.is_string() || object.is_symbol() {
                        val_as_str(object)
                    } else {
                        return Ok(false);
                    };
                let pkg_name = normalize_package_name(&designator);
                return Ok(env.packages.contains_key(&pkg_name)
                    || matches!(
                        pkg_name.as_str(),
                        "COMMON-LISP" | "COMMON-LISP-USER" | "KEYWORD" | "BLISS-EXT"
                    ));
            }
            Ok(!apply_function(predicate, &[object], env)?.is_nil())
        }
        _ => Ok(false),
    }
}

fn package_symbols(env: &Env, package_name: &str, include_inherited: bool) -> Vec<BlissVal> {
    let package_name = normalize_package_name(package_name);
    let mut seen = HashMap::<String, BlissVal>::new();
    if matches!(package_name.as_str(), "COMMON-LISP" | "COMMON-LISP-USER") {
        for idx in 0..4096u32 {
            if let Some(name) = reader::symbol_name(idx) {
                if !name.contains(':') {
                    seen.entry(name.clone())
                        .or_insert(BlissVal::from_symbol_index(idx));
                }
            }
        }
    } else if package_name == "KEYWORD" {
        for idx in 0..4096u32 {
            if let Some(name) = reader::symbol_name(idx) {
                if name.starts_with(':') {
                    seen.entry(name.clone())
                        .or_insert(BlissVal::from_symbol_index(idx));
                }
            }
        }
    }
    if let Some(pkg) = env.packages.get(&package_name) {
        for (name, sym) in &pkg.symbols {
            seen.entry(name.clone()).or_insert(*sym);
        }
        if include_inherited {
            for used in &pkg.uses {
                for sym in package_symbols(env, used, false) {
                    seen.entry(val_as_str(sym)).or_insert(sym);
                }
            }
        }
    }
    seen.into_values().collect()
}

fn load_path_into_env(path: &str, env: &mut Env) -> Result<BlissVal, BlissError> {
    let contents = std::fs::read_to_string(path)
        .map_err(|e| BlissError::FileError(format!("cannot read {}: {}", path, e)))?;
    with_eval_context(env, EvalContext::Load, |env| {
        read_eval_all_env(&contents, env)
    })
}

fn require_module(module: &str, env: &mut Env) -> Result<BlissVal, BlissError> {
    let normalized = module
        .trim_start_matches("KEYWORD:")
        .trim_start_matches(':')
        .trim_matches('"')
        .to_uppercase();
    if normalized == "ASDF" {
        load_path_into_env(&bundled_asdf_path(), env)?;
        return Ok(T);
    }

    let providers = list_to_vec(env.lookup_var("*MODULE-PROVIDER-FUNCTIONS*").unwrap_or(NIL));
    for provider in providers {
        let provided = apply_function(provider, &[arena_str(&normalized)], env)?;
        if !provided.is_nil() {
            return Ok(provided);
        }
    }

    let candidate = format!("{}.lisp", normalized.to_ascii_lowercase());
    load_path_into_env(&candidate, env)
}

#[allow(dead_code)]
fn read_eval_all(source: &str) -> Result<BlissVal, BlissError> {
    let mut env = Env::new(false);
    read_eval_all_env(source, &mut env)
}

fn eval_form(form: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    if form.is_nil() || form == T {
        return Ok(form);
    }
    if form.is_fixnum() || form.is_single_float() || form.is_character() {
        return Ok(form);
    }
    if form.is_heap_object() {
        unsafe {
            let hdr = *(form.as_ptr() as *const ObjectHeader);
            if hdr.type_id() == type_id::SIMPLE_BASE_STRING {
                bliss_stdlib::register_string(form, &val_as_str(form));
                return Ok(form);
            }
        }
    }
    if form.is_symbol() {
        if let Some(expansion) = env.lookup_symbol_macro(form) {
            if expansion == form {
                return Err(BlissError::Internal(format!(
                    "circular symbol macro expansion for {}",
                    sym_name(form)
                )));
            }
            return eval_form(expansion, env);
        }
        if let Some(val) = env.lookup_var_symbol(form) {
            return Ok(val);
        }
        let name = sym_name(form);
        // Keyword symbols are self-evaluating
        if name.starts_with("KEYWORD:") {
            return Ok(form);
        }
        // Check variable environment
        if let Some(val) = env.lookup_var(&name) {
            return Ok(val);
        }
        return Err(BlissError::UnboundVariable(form));
    }
    if form.is_cons() {
        return eval_list(form, env);
    }
    Ok(form)
}

fn canonical_type_name(type_form: BlissVal) -> Result<String, BlissError> {
    if type_form.is_symbol() {
        return Ok(sym_name(type_form)
            .trim_start_matches("COMMON-LISP:")
            .to_string());
    }
    if type_form.is_cons() {
        let (head, rest) = cp(type_form);
        if head.is_symbol() && sym_name(head) == "QUOTE" {
            let (quoted, _) = cp(rest);
            if quoted.is_symbol() {
                return Ok(sym_name(quoted)
                    .trim_start_matches("COMMON-LISP:")
                    .to_string());
            }
        }
    }
    Err(BlissError::TypeError {
        datum: type_form,
        expected: "type specifier".into(),
    })
}

fn value_satisfies_declared_type(type_form: BlissVal, value: BlissVal) -> Result<bool, BlissError> {
    let type_name = canonical_type_name(type_form)?;
    Ok(match type_name.as_str() {
        "T" => true,
        "NIL" | "NULL" => value.is_nil(),
        "BOOLEAN" => value.is_nil() || value == T,
        "SYMBOL" => value.is_symbol(),
        "KEYWORD" => value.is_symbol() && sym_name(value).starts_with("KEYWORD:"),
        "CHARACTER" | "BASE-CHAR" | "STANDARD-CHAR" => value.is_character(),
        "STRING" | "SIMPLE-STRING" | "SIMPLE-BASE-STRING" => value.is_string(),
        "INTEGER" | "FIXNUM" => value.is_fixnum(),
        "FLOAT" | "SINGLE-FLOAT" | "REAL" => value.is_single_float() || value.is_fixnum(),
        "RATIO" => ratio_parts_val(value).is_some(),
        "NUMBER" => {
            value.is_fixnum() || value.is_single_float() || ratio_parts_val(value).is_some()
        }
        "LIST" => value.is_list(),
        "CONS" => value.is_cons(),
        "ATOM" => !value.is_cons(),
        other => {
            if value.is_heap_object() {
                let header = unsafe { *(value.as_ptr() as *const ObjectHeader) };
                match other {
                    "VECTOR" | "SIMPLE-VECTOR" => header.type_id() == type_id::SIMPLE_VECTOR,
                    "HASH-TABLE" => header.type_id() == type_id::HASH_TABLE,
                    _ => false,
                }
            } else {
                false
            }
        }
    })
}

fn eval_when_should_run(situations: BlissVal, env: &Env) -> bool {
    let has_situation = |target: &str| {
        list_to_vec(situations).into_iter().any(|situation| {
            situation.is_symbol()
                && sym_name(situation)
                    .trim_start_matches("KEYWORD:")
                    .trim_start_matches("COMMON-LISP:")
                    == target
        })
    };

    match env.eval_context {
        EvalContext::Load => has_situation("LOAD-TOPLEVEL") || has_situation("EXECUTE"),
        EvalContext::Eval | EvalContext::Repl => has_situation("EXECUTE"),
    }
}

fn eval_form_collecting_values(
    form: BlissVal,
    env: &mut Env,
) -> Result<(BlissVal, Vec<BlissVal>), BlissError> {
    env.clear_mv();
    let primary = eval_form(form, env)?;
    let values = if env.mv_active {
        env.mv.clone()
    } else {
        vec![primary]
    };
    Ok((primary, values))
}

fn cp(val: BlissVal) -> (BlissVal, BlissVal) {
    if !val.is_cons() {
        return (NIL, NIL);
    }
    unsafe {
        let c = val.as_ptr() as *const ConsCell;
        ((*c).car, (*c).cdr)
    }
}

/// A TAGBODY tag is a symbol or an integer. Return its canonical string key,
/// or None if the form is a statement (a cons or other non-tag object).
fn tag_key(form: BlissVal) -> Option<String> {
    if form.is_cons() {
        return None;
    }
    if form.is_symbol() {
        return Some(sym_name(form));
    }
    if form.is_fixnum() {
        return Some(format!("#{}", form.as_fixnum()));
    }
    None
}

fn eval_list(form: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let (car, cdr) = cp(form);
    if car.is_symbol() {
        let name = sym_name(car);

        // Check for macro expansion first
        if let Some(mdef) = env.macros.get(&name).cloned().or_else(|| {
            name.rsplit(':')
                .next()
                .and_then(|bare| env.macros.get(bare).cloned())
        }) {
            let expanded = expand_macro(&mdef, cdr, env)?;
            return eval_form(expanded, env);
        }

        match name.as_str() {
            "QUOTE" => {
                let (q, _) = cp(cdr);
                return Ok(q);
            }
            "BLISS::QUASIQUOTE" => {
                let (template, _) = cp(cdr);
                return eval_quasiquote(template, env);
            }
            "IF" => {
                let (test, r) = cp(cdr);
                let tv = eval_form(test, env)?;
                let (then, er) = cp(r);
                return if !tv.is_nil() {
                    eval_form(then, env)
                } else if er.is_cons() {
                    let (ef, _) = cp(er);
                    eval_form(ef, env)
                } else {
                    Ok(NIL)
                };
            }
            "PROGN" => return eval_progn(cdr, env),
            "DECLARE" => return Ok(NIL),
            "THE" => {
                let (type_form, r) = cp(cdr);
                let (val_form, _) = cp(r);
                let value = eval_form(val_form, env)?;
                if value_satisfies_declared_type(type_form, value)? {
                    return Ok(value);
                }
                return Err(BlissError::TypeError {
                    datum: value,
                    expected: canonical_type_name(type_form)?,
                });
            }
            "LOOP" => return eval_loop(cdr, env),
            "EVAL-WHEN" => {
                let (situations, body) = cp(cdr);
                return if eval_when_should_run(situations, env) {
                    eval_progn(body, env)
                } else {
                    Ok(NIL)
                };
            }
            "BLOCK" => {
                let (name_form, body) = cp(cdr);
                let name = sym_name(name_form);
                let token = next_control_token("__RETURN_FROM__");
                env.block_stack.push((name, token.clone()));
                let result = eval_progn(body, env);
                env.block_stack.pop();
                match result {
                    Err(BlissError::Internal(msg)) if msg == token => {
                        return Ok(take_control_value(&token));
                    }
                    other => return other,
                }
            }
            "RETURN-FROM" => {
                let (name_form, rest) = cp(cdr);
                let (val_form, _) = cp(rest);
                let name = sym_name(name_form);
                let value = eval_form(val_form, env)?;
                if let Some((_, token)) = env
                    .block_stack
                    .iter()
                    .rev()
                    .find(|(block_name, _)| block_name == &name)
                {
                    store_control_value(token, value);
                    return Err(BlissError::Internal(token.clone()));
                }
                return Err(BlissError::Internal(format!(
                    "RETURN-FROM: no block named {} is currently visible",
                    name
                )));
            }
            "RETURN" => {
                let (val_form, _) = cp(cdr);
                let value = eval_form(val_form, env)?;
                if let Some((_, token)) = env
                    .block_stack
                    .iter()
                    .rev()
                    .find(|(block_name, _)| block_name == "NIL")
                {
                    store_control_value(token, value);
                    return Err(BlissError::Internal(token.clone()));
                }
                return Err(BlissError::Internal(
                    "RETURN: no block named NIL is currently visible".into(),
                ));
            }
            "CATCH" => {
                let (tag_form, body) = cp(cdr);
                let tag = val_as_str(eval_form(tag_form, env)?);
                let token = next_control_token("__THROW__");
                env.catch_stack.push((tag, token.clone()));
                let result = eval_progn(body, env);
                env.catch_stack.pop();
                match result {
                    Err(BlissError::Internal(msg)) if msg == token => {
                        return Ok(take_control_value(&token));
                    }
                    other => return other,
                }
            }
            "THROW" => {
                let (tag_form, rest) = cp(cdr);
                let (val_form, _) = cp(rest);
                let tag = val_as_str(eval_form(tag_form, env)?);
                let value = eval_form(val_form, env)?;
                if let Some((_, token)) = env
                    .catch_stack
                    .iter()
                    .rev()
                    .find(|(catch_tag, _)| catch_tag == &tag)
                {
                    store_control_value(token, value);
                    return Err(BlissError::Internal(token.clone()));
                }
                return Err(BlissError::Internal(format!("uncaught throw to {}", tag)));
            }
            "TAGBODY" => {
                // (tagbody {tag | statement}*)
                // Tags are symbols or integers; statements are forms evaluated in
                // order. GO transfers control to a tag; TAGBODY returns NIL.
                let items = list_to_vec(cdr);
                let token = next_control_token("__GO__");
                // Record tag name -> statement index (index just after the tag).
                let mut tag_index: HashMap<String, usize> = HashMap::new();
                let base = env.tag_stack.len();
                for (i, item) in items.iter().enumerate() {
                    if let Some(name) = tag_key(*item) {
                        tag_index.entry(name.clone()).or_insert(i);
                        env.tag_stack.push((name, token.clone()));
                    }
                }
                let mut pc = 0usize;
                let result: Result<(), BlissError> = loop {
                    if pc >= items.len() {
                        break Ok(());
                    }
                    let item = items[pc];
                    if tag_key(item).is_some() {
                        pc += 1;
                        continue;
                    }
                    match eval_form(item, env) {
                        Ok(_) => {
                            pc += 1;
                        }
                        Err(BlissError::Internal(msg)) if msg == token => {
                            let target = val_as_str(take_control_value(&token));
                            match tag_index.get(&target) {
                                Some(idx) => {
                                    pc = *idx;
                                }
                                None => break Err(BlissError::Internal(msg)),
                            }
                        }
                        Err(e) => break Err(e),
                    }
                };
                env.tag_stack.truncate(base);
                match result {
                    Ok(()) => return Ok(NIL),
                    Err(e) => return Err(e),
                }
            }
            "GO" => {
                // (go tag) — tag is not evaluated.
                let (tag_form, _) = cp(cdr);
                let name = match tag_key(tag_form) {
                    Some(n) => n,
                    None => {
                        return Err(BlissError::Internal(
                            "GO: tag must be a symbol or integer".into(),
                        ));
                    }
                };
                if let Some((_, token)) = env
                    .tag_stack
                    .iter()
                    .rev()
                    .find(|(tag_name, _)| tag_name == &name)
                {
                    let token = token.clone();
                    store_control_value(&token, arena_str(&name));
                    return Err(BlissError::Internal(token));
                }
                return Err(BlissError::Internal(format!("GO: no such tag {}", name)));
            }
            "UNWIND-PROTECT" => {
                // (unwind-protect protected cleanup...) — cleanup runs whether the
                // protected form returns normally or exits non-locally.
                let (protected, cleanup) = cp(cdr);
                let result = eval_form(protected, env);
                match result {
                    Ok(v) => {
                        let saved_mv = env.mv.clone();
                        let saved_mv_active = env.mv_active;
                        eval_progn(cleanup, env)?;
                        env.mv = saved_mv;
                        env.mv_active = saved_mv_active;
                        return Ok(v);
                    }
                    Err(e) => match eval_progn(cleanup, env) {
                        Ok(_) => return Err(e),
                        Err(cleanup_exit) => return Err(cleanup_exit),
                    },
                }
            }
            "PRINT" => {
                let (a, _) = cp(cdr);
                let v = eval_form(a, env)?;
                println!("\n{}", format_val(v));
                return Ok(v);
            }
            "PRINC" => {
                let (a, _) = cp(cdr);
                let v = eval_form(a, env)?;
                let mut s = String::new();
                princ_val(v, &mut s);
                print!("{}", s);
                return Ok(v);
            }
            "TERPRI" => {
                println!();
                return Ok(NIL);
            }
            "FRESH-LINE" => {
                println!();
                return Ok(T);
            }
            "+" => return eval_arith(cdr, env, 0, 0.0, |a, b| a + b, bigrat_add),
            "-" => return eval_arith_sub(cdr, env),
            "*" => return eval_arith(cdr, env, 1, 1.0, |a, b| a * b, bigrat_mul),
            "/" => return eval_arith_div(cdr, env),
            "CONS" => {
                let (af, r) = cp(cdr);
                let (bf, _) = cp(r);
                let a = eval_form(af, env)?;
                let b = eval_form(bf, env)?;
                return Ok(arena_cons(a, b));
            }
            "LIST" => {
                let mut elems = Vec::new();
                let mut c = cdr;
                while c.is_cons() {
                    let (ef, r) = cp(c);
                    elems.push(eval_form(ef, env)?);
                    c = r;
                }
                return Ok(vec_to_list(&elems));
            }
            "CAR" | "FIRST" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                if v.is_nil() {
                    return Ok(NIL);
                }
                if v.is_cons() {
                    let (a, _) = cp(v);
                    return Ok(a);
                }
                return Err(BlissError::TypeError {
                    datum: v,
                    expected: "list".into(),
                });
            }
            "CDR" | "REST" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                if v.is_nil() {
                    return Ok(NIL);
                }
                if v.is_cons() {
                    let (_, d) = cp(v);
                    return Ok(d);
                }
                return Err(BlissError::TypeError {
                    datum: v,
                    expected: "list".into(),
                });
            }
            "SECOND" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                if v.is_nil() {
                    return Ok(NIL);
                }
                if v.is_cons() {
                    let (_, d) = cp(v);
                    if d.is_cons() {
                        let (a, _) = cp(d);
                        return Ok(a);
                    }
                }
                return Ok(NIL);
            }
            "THIRD" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                if v.is_nil() {
                    return Ok(NIL);
                }
                let elems = list_to_vec(v);
                return Ok(if elems.len() >= 3 { elems[2] } else { NIL });
            }
            "ATOM" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                return Ok(if v.is_cons() { NIL } else { T });
            }
            "NULL" | "NOT" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                return Ok(if v.is_nil() { T } else { NIL });
            }
            "CONSP" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                return Ok(if v.is_cons() { T } else { NIL });
            }
            "LISTP" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                return Ok(if v.is_list() { T } else { NIL });
            }
            "NUMBERP" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                return Ok(if v.is_fixnum() || v.is_single_float() {
                    T
                } else {
                    NIL
                });
            }
            "STRINGP" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                return Ok(if v.is_string() { T } else { NIL });
            }
            "BOUNDP" => {
                let (sf, _) = cp(cdr);
                let sym = eval_form(sf, env)?;
                let name = if sym.is_symbol() {
                    sym_name(sym)
                } else {
                    val_as_str(sym)
                };
                return Ok(if env.lookup_var(&name).is_some() {
                    T
                } else {
                    NIL
                });
            }
            "SYMBOLP" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                return Ok(if v.is_symbol() { T } else { NIL });
            }
            "TYPEP" => {
                let (obj_form, r) = cp(cdr);
                let (type_form, _) = cp(r);
                let obj = eval_form(obj_form, env)?;
                let raw_type_spec = eval_form(type_form, env)?;
                let matches = typep_matches(env, obj, raw_type_spec)?;
                return Ok(if matches { T } else { NIL });
            }
            "EQ" | "EQL" => {
                let (af, r) = cp(cdr);
                let (bf, _) = cp(r);
                let a = eval_form(af, env)?;
                let b = eval_form(bf, env)?;
                return Ok(if a == b { T } else { NIL });
            }
            "EQUAL" | "EQUALP" => {
                let (af, r) = cp(cdr);
                let (bf, _) = cp(r);
                let a = eval_form(af, env)?;
                let b = eval_form(bf, env)?;
                return Ok(if vals_equal(a, b) { T } else { NIL });
            }
            "=" => {
                let (af, r) = cp(cdr);
                let (bf, _) = cp(r);
                let a = eval_form(af, env)?;
                let b = eval_form(bf, env)?;
                return Ok(if numeric_cmp(a, b)? == Ordering::Equal {
                    T
                } else {
                    NIL
                });
            }
            "<" => {
                return eval_cmp(cdr, env, |o| o == Ordering::Less);
            }
            ">" => {
                return eval_cmp(cdr, env, |o| o == Ordering::Greater);
            }
            "<=" => {
                return eval_cmp(cdr, env, |o| o != Ordering::Greater);
            }
            ">=" => {
                return eval_cmp(cdr, env, |o| o != Ordering::Less);
            }
            "/=" => {
                let (af, r) = cp(cdr);
                let (bf, _) = cp(r);
                let a = eval_form(af, env)?;
                let b = eval_form(bf, env)?;
                return Ok(if numeric_cmp(a, b)? != Ordering::Equal {
                    T
                } else {
                    NIL
                });
            }
            "AND" => {
                let mut result = T;
                let mut c = cdr;
                while c.is_cons() {
                    let (f, r) = cp(c);
                    result = eval_form(f, env)?;
                    if result.is_nil() {
                        return Ok(NIL);
                    }
                    c = r;
                }
                return Ok(result);
            }
            "OR" => {
                let mut c = cdr;
                while c.is_cons() {
                    let (f, r) = cp(c);
                    let v = eval_form(f, env)?;
                    if !v.is_nil() {
                        return Ok(v);
                    }
                    c = r;
                }
                return Ok(NIL);
            }
            "WHEN" => {
                let (test, body) = cp(cdr);
                let tv = eval_form(test, env)?;
                if !tv.is_nil() {
                    return eval_progn(body, env);
                }
                return Ok(NIL);
            }
            "UNLESS" => {
                let (test, body) = cp(cdr);
                let tv = eval_form(test, env)?;
                if tv.is_nil() {
                    return eval_progn(body, env);
                }
                return Ok(NIL);
            }
            "DESTRUCTURING-BIND" => {
                let (pattern, rest) = cp(cdr);
                let (value_form, body) = cp(rest);
                let value = eval_form(value_form, env)?;
                let mut child_env = env.child();
                bind_pattern_value(pattern, value, &mut child_env)?;
                return eval_progn(body, &mut child_env);
            }
            "COND" => {
                let mut c = cdr;
                while c.is_cons() {
                    let (clause, rest) = cp(c);
                    let (test, body) = cp(clause);
                    let tv = eval_form(test, env)?;
                    if !tv.is_nil() {
                        if body.is_nil() {
                            return Ok(tv);
                        }
                        return eval_progn(body, env);
                    }
                    c = rest;
                }
                return Ok(NIL);
            }
            "VALUES" => {
                let mut vals = Vec::new();
                let mut c = cdr;
                while c.is_cons() {
                    let (af, r) = cp(c);
                    vals.push(eval_form(af, env)?);
                    c = r;
                }
                if vals.is_empty() {
                    env.set_mv(Vec::new());
                    return Ok(NIL);
                }
                env.set_mv(vals.clone());
                return Ok(vals[0]);
            }
            "FORMAT" => return eval_format(cdr, env),
            "ERROR" => {
                let args = list_to_vec(cdr);
                if args.is_empty() {
                    return Err(BlissError::Internal("ERROR".into()));
                }
                let control = eval_form(args[0], env)?;
                let mut format_args = Vec::new();
                for arg in &args[1..] {
                    format_args.push(eval_form(*arg, env)?);
                }
                let message = if control.is_string() && !format_args.is_empty() {
                    simple_format_message(&val_as_str(control), &format_args)
                } else {
                    val_as_str(control)
                };
                let condition = if args.len() == 1 && !control.is_string() {
                    control
                } else {
                    make_simple_error_condition(arena_str(&message), env)?
                };
                match signal_condition_object(condition, env) {
                    Ok(_) => return Err(BlissError::Internal(format!("ERROR: {}", message))),
                    Err(error) => return Err(error),
                }
            }
            "LET" => return eval_let(cdr, env, false),
            "LET*" => return eval_let(cdr, env, true),
            "SETQ" => {
                let mut c = cdr;
                let mut result = NIL;
                while c.is_cons() {
                    let (sym_form, r) = cp(c);
                    let (val_form, r2) = cp(r);
                    if sym_form.is_symbol() {
                        if let Some(expansion) = env.lookup_symbol_macro(sym_form) {
                            let setf_form = arena_cons(
                                resolve_sym("SETF").unwrap_or(NIL),
                                arena_cons(expansion, arena_cons(val_form, NIL)),
                            );
                            result = eval_form(setf_form, env)?;
                            c = r2;
                            continue;
                        }
                    }
                    let val = eval_form(val_form, env)?;
                    if sym_form.is_symbol() {
                        env.set_var_symbol(sym_form, val);
                    } else {
                        env.set_var(&sym_name(sym_form), val);
                    }
                    result = val;
                    c = r2;
                }
                return Ok(result);
            }
            "SETF" => {
                // (setf place value place value ...) — symbol places behave like
                // SETQ; a handful of common accessor places are supported by
                // mutating the target in place. Other places error clearly.
                let mut c = cdr;
                let mut result = NIL;
                while c.is_cons() {
                    let (place, r) = cp(c);
                    let (val_form, r2) = cp(r);
                    let val = eval_form(val_form, env)?;
                    if place.is_symbol() {
                        if let Some(expansion) = env.lookup_symbol_macro(place) {
                            let setf_form = arena_cons(
                                resolve_sym("SETF").unwrap_or(NIL),
                                arena_cons(expansion, arena_cons(val_form, NIL)),
                            );
                            result = eval_form(setf_form, env)?;
                            c = r2;
                            continue;
                        }
                        env.set_var_symbol(place, val);
                    } else if place.is_cons() {
                        let (accessor, aargs) = cp(place);
                        let acc = if accessor.is_symbol() {
                            sym_name(accessor)
                        } else {
                            String::new()
                        };
                        let (tgt_form, _) = cp(aargs);
                        match acc.as_str() {
                            "CAR" | "FIRST" => {
                                let tgt = eval_form(tgt_form, env)?;
                                if tgt.is_cons() {
                                    unsafe {
                                        (*(tgt.as_ptr() as *mut ConsCell)).car = val;
                                    }
                                } else {
                                    return Err(BlissError::TypeError {
                                        datum: tgt,
                                        expected: "cons".into(),
                                    });
                                }
                            }
                            "CDR" | "REST" => {
                                let tgt = eval_form(tgt_form, env)?;
                                if tgt.is_cons() {
                                    unsafe {
                                        (*(tgt.as_ptr() as *mut ConsCell)).cdr = val;
                                    }
                                } else {
                                    return Err(BlissError::TypeError {
                                        datum: tgt,
                                        expected: "cons".into(),
                                    });
                                }
                            }
                            "GETHASH" => {
                                // (setf (gethash key table) val)
                                let key = eval_form(tgt_form, env)?;
                                let (tbl_form, _) = cp(cp(aargs).1);
                                let tbl = eval_form(tbl_form, env)?;
                                bliss_stdlib::set_gethash(key, tbl, val)?;
                            }
                            other => {
                                let reader_slot = env.classes.values().find_map(|class| {
                                    class.slots.iter().find_map(|slot| {
                                        let matches_reader = slot
                                            .accessor
                                            .as_ref()
                                            .map(|acc| acc == other)
                                            .unwrap_or(false)
                                            || slot.readers.iter().any(|reader| reader == other)
                                            || slot.writers.iter().any(|writer| writer == other);
                                        if matches_reader {
                                            Some(slot.name.clone())
                                        } else {
                                            None
                                        }
                                    })
                                });
                                if let Some(slot_name) = reader_slot {
                                    let tgt = eval_form(tgt_form, env)?;
                                    write_slot_value(
                                        tgt,
                                        resolve_sym(&slot_name).unwrap_or(NIL),
                                        val,
                                        env,
                                    )?;
                                } else {
                                    return Err(BlissError::Internal(format!(
                                        "SETF: unsupported place ({} ...)",
                                        other
                                    )));
                                }
                            }
                        }
                    }
                    result = val;
                    c = r2;
                }
                return Ok(result);
            }
            "DEFUN" => return eval_defun(cdr, env),
            "FLET" | "LABELS" => return eval_flet(cdr, env),
            "DEFMACRO" => return eval_defmacro(cdr, env),
            "DEFINE-SYMBOL-MACRO" => return eval_define_symbol_macro(cdr, env),
            "DEFINE-COMPILER-MACRO" => return eval_define_compiler_macro(cdr, env),
            "MACROLET" => return eval_macrolet(cdr, env),
            "SYMBOL-MACROLET" => return eval_symbol_macrolet(cdr, env),
            "MACROEXPAND-1" => return eval_macroexpand(cdr, env, true),
            "MACROEXPAND" => return eval_macroexpand(cdr, env, false),
            "DEFCLASS" => return eval_defclass(cdr, env),
            "DEFGENERIC" => return eval_defgeneric(cdr, env),
            "DEFMETHOD" => return eval_defmethod(cdr, env),
            "MAKE-INSTANCE" => return eval_make_instance(cdr, env),
            "FUNCTION" => {
                let (name_form, _) = cp(cdr);
                if name_form.is_symbol() {
                    let fn_name = sym_name(name_form);
                    if env.funs.contains_key(&fn_name) {
                        return Ok(name_form); // return the symbol as a function designator
                    }
                }
                // (function (lambda (params) body...)) — create a closure
                if name_form.is_cons() {
                    let (lh, lr) = cp(name_form);
                    if lh.is_symbol() && sym_name(lh) == "LAMBDA" {
                        let (params_form, body) = cp(lr);
                        // Capture the current lexical environment
                        let closure = Closure {
                            params_form,
                            body,
                            captured_frame: Rc::clone(&env.frame),
                        };
                        let id = next_closure_id();
                        env.closures.borrow_mut().insert(id, closure);
                        // Return a tagged closure reference as (BLISS::CLOSURE . id)
                        let closure_sym = resolve_sym("BLISS::CLOSURE").unwrap_or(NIL);
                        return Ok(arena_cons(closure_sym, BlissVal::from_fixnum(id as i64)));
                    }
                }
                return Ok(name_form);
            }
            "LAMBDA" => {
                // Bare (lambda (params) body...) — create a closure
                let (params_form, body) = cp(cdr);
                let closure = Closure {
                    params_form,
                    body,
                    captured_frame: Rc::clone(&env.frame),
                };
                let id = next_closure_id();
                env.closures.borrow_mut().insert(id, closure);
                let closure_sym = resolve_sym("BLISS::CLOSURE").unwrap_or(NIL);
                return Ok(arena_cons(closure_sym, BlissVal::from_fixnum(id as i64)));
            }
            "FUNCALL" => {
                let (fn_form, args_form) = cp(cdr);
                let fn_val = eval_form(fn_form, env)?;
                let mut args = Vec::new();
                let mut c = args_form;
                while c.is_cons() {
                    let (af, r) = cp(c);
                    args.push(eval_form(af, env)?);
                    c = r;
                }
                return apply_function(fn_val, &args, env);
            }
            "APPLY" => {
                let (fn_form, rest) = cp(cdr);
                let fn_val = eval_form(fn_form, env)?;
                let rest_items = list_to_vec(rest);
                let mut args = Vec::new();
                for (i, item) in rest_items.iter().enumerate() {
                    let v = eval_form(*item, env)?;
                    if i == rest_items.len() - 1 {
                        // Last arg should be a list to spread
                        let spread = list_to_vec(v);
                        args.extend(spread);
                    } else {
                        args.push(v);
                    }
                }
                return apply_function(fn_val, &args, env);
            }
            "LENGTH" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                return Ok(BlissVal::from_fixnum(bliss_stdlib::length(v)? as i64));
            }
            "APPEND" => {
                let mut all = Vec::new();
                let items = list_to_vec(cdr);
                if items.is_empty() {
                    return Ok(NIL);
                }
                for (i, item_form) in items.iter().enumerate() {
                    let v = eval_form(*item_form, env)?;
                    if i == items.len() - 1 && v.is_nil() {
                        continue;
                    }
                    all.extend(list_to_vec(v));
                }
                return Ok(vec_to_list(&all));
            }
            "REVERSE" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                let mut elems = list_to_vec(v);
                elems.reverse();
                return Ok(vec_to_list(&elems));
            }
            "NTH" => {
                let (nf, r) = cp(cdr);
                let (lf, _) = cp(r);
                let n = eval_form(nf, env)?;
                let l = eval_form(lf, env)?;
                let idx = num_val(n)? as usize;
                let elems = list_to_vec(l);
                return Ok(if idx < elems.len() { elems[idx] } else { NIL });
            }
            "MAKE-HASH-TABLE" => {
                // (make-hash-table &key test size ...) — honor :test, evaluate
                // and ignore the rest. Backed by the real stdlib hash table.
                let mut test = bliss_stdlib::HashTest::Eql;
                let mut c = cdr;
                while c.is_cons() {
                    let (kw, r) = cp(c);
                    if !r.is_cons() {
                        break;
                    }
                    let (vf, r2) = cp(r);
                    let v = eval_form(vf, env)?;
                    if kw.is_symbol() {
                        let kn = sym_name(kw);
                        if kn.strip_prefix("KEYWORD:").unwrap_or(&kn) == "TEST" {
                            let tn = sym_name(v);
                            let tb = tn.strip_prefix("KEYWORD:").unwrap_or(&tn).to_uppercase();
                            test = match tb.as_str() {
                                "EQ" => bliss_stdlib::HashTest::Eq,
                                "EQUAL" => bliss_stdlib::HashTest::Equal,
                                "EQUALP" => bliss_stdlib::HashTest::Equalp,
                                _ => bliss_stdlib::HashTest::Eql,
                            };
                        }
                    }
                    c = r2;
                }
                let opts = bliss_stdlib::MakeHashTableOptions {
                    test,
                    ..Default::default()
                };
                return bliss_stdlib::make_hash_table(&opts);
            }
            "GETHASH" => {
                // (gethash key table &optional default) -> value; sets the
                // second value to the present-p flag.
                let (key_form, r) = cp(cdr);
                let (tbl_form, r2) = cp(r);
                let key = eval_form(key_form, env)?;
                let tbl = eval_form(tbl_form, env)?;
                let default = if r2.is_cons() {
                    eval_form(cp(r2).0, env)?
                } else {
                    NIL
                };
                let (val, present) = bliss_stdlib::gethash(key, tbl, default)?;
                env.set_mv(vec![val, if present { T } else { NIL }]);
                return Ok(val);
            }
            "REMHASH" => {
                let (key_form, r) = cp(cdr);
                let (tbl_form, _) = cp(r);
                let key = eval_form(key_form, env)?;
                let tbl = eval_form(tbl_form, env)?;
                let removed = bliss_stdlib::remhash(key, tbl)?;
                return Ok(if removed { T } else { NIL });
            }
            "HASH-TABLE-COUNT" => {
                let (tbl_form, _) = cp(cdr);
                let tbl = eval_form(tbl_form, env)?;
                let n = bliss_stdlib::hash_table_count(tbl)?;
                return Ok(BlissVal::from_fixnum(n as i64));
            }
            "MAPCAR" => {
                let (fn_form, r) = cp(cdr);
                let fn_val = eval_form(fn_form, env)?;
                let (list_form, _) = cp(r);
                let list = eval_form(list_form, env)?;
                let elems = list_to_vec(list);
                let mut results = Vec::new();
                for e in &elems {
                    results.push(apply_function(fn_val, &[*e], env)?);
                }
                return Ok(vec_to_list(&results));
            }
            "MAP" => {
                let args = list_to_vec(cdr);
                if args.len() < 3 {
                    return Err(BlissError::Internal(
                        "MAP requires a result type, function, and sequence".into(),
                    ));
                }
                let result_type = eval_form(args[0], env)?;
                let fn_val = eval_form(args[1], env)?;
                let mut seqs: Vec<Vec<BlissVal>> = Vec::new();
                for seq_form in &args[2..] {
                    let seq = eval_form(*seq_form, env)?;
                    seqs.push(if seq.is_string() {
                        val_as_str(seq)
                            .chars()
                            .map(BlissVal::from_char)
                            .collect::<Vec<_>>()
                    } else {
                        list_to_vec(seq)
                    });
                }
                let len = seqs.iter().map(Vec::len).min().unwrap_or(0);
                let mut results = Vec::with_capacity(len);
                for i in 0..len {
                    let call_args = seqs.iter().map(|seq| seq[i]).collect::<Vec<_>>();
                    results.push(apply_function(fn_val, &call_args, env)?);
                }
                let result_name = if result_type.is_symbol() {
                    symbol_bare_name(&sym_name(result_type))
                } else {
                    val_as_str(result_type).to_uppercase()
                };
                return Ok(match result_name.as_str() {
                    "NIL" => NIL,
                    "LIST" => vec_to_list(&results),
                    "STRING" | "SIMPLE-STRING" | "BASE-STRING" => {
                        let s = results.iter().map(|v| v.as_char()).collect::<String>();
                        arena_str(&s)
                    }
                    _ => vec_to_list(&results),
                });
            }
            "FIND" => {
                let (item_form, r) = cp(cdr);
                let (seq_form, mut rest) = cp(r);
                let item = eval_form(item_form, env)?;
                let seq = eval_form(seq_form, env)?;
                let mut test = NIL;
                let mut key = None;
                let mut start = 0usize;
                let mut end = None;
                let mut from_end = false;
                while rest.is_cons() {
                    let (kw, r2) = cp(rest);
                    if !r2.is_cons() {
                        break;
                    }
                    let (value_form, r3) = cp(r2);
                    let value = eval_form(value_form, env)?;
                    if kw.is_symbol() {
                        let name = sym_name(kw);
                        match name.strip_prefix("KEYWORD:").unwrap_or(&name) {
                            "TEST" => test = value,
                            "KEY" => key = Some(value),
                            "START" => start = num_val(value)? as usize,
                            "END" => end = Some(num_val(value)? as usize),
                            "FROM-END" => from_end = !value.is_nil(),
                            _ => {}
                        }
                    }
                    rest = r3;
                }
                return bliss_stdlib::find(item, seq, test, key, start, end, from_end);
            }
            "POSITION" => {
                let (item_form, r) = cp(cdr);
                let (seq_form, mut rest) = cp(r);
                let item = eval_form(item_form, env)?;
                let seq = eval_form(seq_form, env)?;
                let mut test = NIL;
                let mut key = None;
                let mut start = 0usize;
                let mut end = None;
                let mut from_end = false;
                while rest.is_cons() {
                    let (kw, r2) = cp(rest);
                    if !r2.is_cons() {
                        break;
                    }
                    let (value_form, r3) = cp(r2);
                    let value = eval_form(value_form, env)?;
                    if kw.is_symbol() {
                        let name = sym_name(kw);
                        match name.strip_prefix("KEYWORD:").unwrap_or(&name) {
                            "TEST" => test = value,
                            "KEY" => key = Some(value),
                            "START" => start = num_val(value)? as usize,
                            "END" => end = Some(num_val(value)? as usize),
                            "FROM-END" => from_end = !value.is_nil(),
                            _ => {}
                        }
                    }
                    rest = r3;
                }
                return bliss_stdlib::position(item, seq, test, key, start, end, from_end);
            }
            "COUNT" => {
                let (item_form, r) = cp(cdr);
                let (seq_form, mut rest) = cp(r);
                let item = eval_form(item_form, env)?;
                let seq = eval_form(seq_form, env)?;
                let mut test = NIL;
                let mut key = None;
                let mut start = 0usize;
                let mut end = None;
                while rest.is_cons() {
                    let (kw, r2) = cp(rest);
                    if !r2.is_cons() {
                        break;
                    }
                    let (value_form, r3) = cp(r2);
                    let value = eval_form(value_form, env)?;
                    if kw.is_symbol() {
                        let name = sym_name(kw);
                        match name.strip_prefix("KEYWORD:").unwrap_or(&name) {
                            "TEST" => test = value,
                            "KEY" => key = Some(value),
                            "START" => start = num_val(value)? as usize,
                            "END" => end = Some(num_val(value)? as usize),
                            _ => {}
                        }
                    }
                    rest = r3;
                }
                return bliss_stdlib::count(item, seq, test, key, start, end);
            }
            "MEMBER" => {
                let (item_f, r) = cp(cdr);
                let (list_f, _) = cp(r);
                let item = eval_form(item_f, env)?;
                let list = eval_form(list_f, env)?;
                let mut c = list;
                while c.is_cons() {
                    let (car, cdr_val) = cp(c);
                    if vals_equal(car, item) {
                        return Ok(c);
                    }
                    c = cdr_val;
                }
                return Ok(NIL);
            }
            "ASSOC" => {
                let (key_f, r) = cp(cdr);
                let (alist_f, _) = cp(r);
                let key = eval_form(key_f, env)?;
                let alist = eval_form(alist_f, env)?;
                let mut c = alist;
                while c.is_cons() {
                    let (pair, rest) = cp(c);
                    if pair.is_cons() {
                        let (k, _) = cp(pair);
                        if vals_equal(k, key) {
                            return Ok(pair);
                        }
                    }
                    c = rest;
                }
                return Ok(NIL);
            }
            "CONCATENATE" => {
                // Delegate to stdlib so CLI sequence behavior matches the
                // same implementation used by lower-level stage-3 tests.
                let (type_form, rest) = cp(cdr);
                let result_type = eval_form(type_form, env)?;
                let mut sequences = Vec::new();
                let mut c = rest;
                while c.is_cons() {
                    let (sf, r) = cp(c);
                    sequences.push(eval_form(sf, env)?);
                    c = r;
                }
                return bliss_stdlib::concatenate(result_type, &sequences);
            }
            "SUBSEQ" => {
                let (seq_form, rest) = cp(cdr);
                let (start_form, rest2) = cp(rest);
                let seq = eval_form(seq_form, env)?;
                let start = num_val(eval_form(start_form, env)?)? as usize;
                let end = if rest2.is_cons() {
                    Some(num_val(eval_form(cp(rest2).0, env)?)? as usize)
                } else {
                    None
                };
                return bliss_stdlib::subseq(seq, start, end);
            }
            "SORT" => {
                let (seq_form, rest) = cp(cdr);
                let (pred_form, rest2) = cp(rest);
                let seq = eval_form(seq_form, env)?;
                let predicate = eval_form(pred_form, env)?;
                let key = if rest2.is_cons() {
                    let (kw_form, rest3) = cp(rest2);
                    if kw_form.is_symbol()
                        && symbol_bare_name(&sym_name(kw_form)).eq_ignore_ascii_case("KEY")
                        && rest3.is_cons()
                    {
                        Some(eval_form(cp(rest3).0, env)?)
                    } else {
                        None
                    }
                } else {
                    None
                };
                return bliss_stdlib::sort(seq, predicate, key);
            }
            "STABLE-SORT" => {
                let (seq_form, rest) = cp(cdr);
                let (pred_form, rest2) = cp(rest);
                let seq = eval_form(seq_form, env)?;
                let predicate = eval_form(pred_form, env)?;
                let key = if rest2.is_cons() {
                    let (kw_form, rest3) = cp(rest2);
                    if kw_form.is_symbol()
                        && symbol_bare_name(&sym_name(kw_form)).eq_ignore_ascii_case("KEY")
                        && rest3.is_cons()
                    {
                        Some(eval_form(cp(rest3).0, env)?)
                    } else {
                        None
                    }
                } else {
                    None
                };
                return bliss_stdlib::stable_sort(seq, predicate, key);
            }
            "PARSE-NAMESTRING" => {
                let (thing_form, rest) = cp(cdr);
                let thing = eval_form(thing_form, env)?;
                let host = if rest.is_cons() {
                    Some(eval_form(cp(rest).0, env)?)
                } else {
                    None
                };
                let (pathname, position) = bliss_stdlib::parse_namestring(thing, host, None)?;
                env.set_mv(vec![pathname, BlissVal::from_fixnum(position as i64)]);
                return Ok(pathname);
            }
            "NAMESTRING" => {
                let (pathname_form, _) = cp(cdr);
                let pathname = eval_form(pathname_form, env)?;
                return bliss_stdlib::namestring(pathname);
            }
            "PATHNAME-NAME" => {
                let (pathname_form, _) = cp(cdr);
                let pathname = eval_form(pathname_form, env)?;
                return Ok(bliss_stdlib::pathname_name(pathname));
            }
            "PATHNAME-TYPE" => {
                let (pathname_form, _) = cp(cdr);
                let pathname = eval_form(pathname_form, env)?;
                return Ok(bliss_stdlib::pathname_type(pathname));
            }
            "PATHNAME-DIRECTORY" => {
                let (pathname_form, _) = cp(cdr);
                let pathname = eval_form(pathname_form, env)?;
                return Ok(bliss_stdlib::pathname_directory(pathname));
            }
            "PATHNAME-HOST" => {
                let (pathname_form, _) = cp(cdr);
                let pathname = eval_form(pathname_form, env)?;
                return Ok(bliss_stdlib::pathname_host(pathname));
            }
            "PATHNAME-DEVICE" => {
                let (pathname_form, _) = cp(cdr);
                let pathname = eval_form(pathname_form, env)?;
                return Ok(bliss_stdlib::pathname_device(pathname));
            }
            "PATHNAME-VERSION" => {
                let (pathname_form, _) = cp(cdr);
                let pathname = eval_form(pathname_form, env)?;
                return Ok(bliss_stdlib::pathname_version(pathname));
            }
            "STRING" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                return Ok(arena_str(&val_as_str(v)));
            }
            "WRITE-TO-STRING" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                return Ok(arena_str(&format_val(v)));
            }
            "ABS" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                if v.is_fixnum() {
                    return Ok(BlissVal::from_fixnum(v.as_fixnum().abs()));
                }
                if v.is_single_float() {
                    return Ok(BlissVal::from_single_float(v.as_single_float().abs()));
                }
                return Err(BlissError::TypeError {
                    datum: v,
                    expected: "number".into(),
                });
            }
            "MIN" => {
                let args = eval_args(cdr, env)?;
                if args.is_empty() {
                    return Err(BlissError::Internal(
                        "MIN requires at least one argument".into(),
                    ));
                }
                let mut min = num_val(args[0])?;
                let mut is_f = args[0].is_single_float();
                for a in &args[1..] {
                    let v = num_val(*a)?;
                    let is_single_float = a.is_single_float();
                    if v < min {
                        min = v;
                    }
                    if is_single_float {
                        is_f = true;
                    }
                }
                return Ok(if is_f {
                    BlissVal::from_single_float(min as f32)
                } else {
                    BlissVal::from_fixnum(min as i64)
                });
            }
            "MAX" => {
                let args = eval_args(cdr, env)?;
                if args.is_empty() {
                    return Err(BlissError::Internal(
                        "MAX requires at least one argument".into(),
                    ));
                }
                let mut max = num_val(args[0])?;
                let mut is_f = args[0].is_single_float();
                for a in &args[1..] {
                    let v = num_val(*a)?;
                    let is_single_float = a.is_single_float();
                    if v > max {
                        max = v;
                    }
                    if is_single_float {
                        is_f = true;
                    }
                }
                return Ok(if is_f {
                    BlissVal::from_single_float(max as f32)
                } else {
                    BlissVal::from_fixnum(max as i64)
                });
            }
            "FLOOR" => return eval_floor(cdr, env),
            "MOD" | "REM" => {
                let (af, r) = cp(cdr);
                let (bf, _) = cp(r);
                let a = eval_form(af, env)?;
                let b = eval_form(bf, env)?;
                let av = num_val(a)? as i64;
                let bv = num_val(b)? as i64;
                if bv == 0 {
                    return Err(BlissError::ArithmeticError("division by zero".into()));
                }
                return Ok(BlissVal::from_fixnum(av % bv));
            }
            "TRUNCATE" => {
                let (af, r) = cp(cdr);
                let a = eval_form(af, env)?;
                let av = num_val(a)?;
                if r.is_cons() {
                    let (bf, _) = cp(r);
                    let b = eval_form(bf, env)?;
                    let bv = num_val(b)?;
                    if bv == 0.0 {
                        return Err(BlissError::ArithmeticError("division by zero".into()));
                    }
                    let q = (av / bv).trunc() as i64;
                    let rem = av - (q as f64) * bv;
                    env.set_mv(vec![
                        BlissVal::from_fixnum(q),
                        BlissVal::from_single_float(rem as f32),
                    ]);
                    return Ok(BlissVal::from_fixnum(q));
                }
                return Ok(BlissVal::from_fixnum(av as i64));
            }
            "CEILING" => {
                let (af, r) = cp(cdr);
                let a = eval_form(af, env)?;
                let av = num_val(a)?;
                if r.is_cons() {
                    let (bf, _) = cp(r);
                    let b = eval_form(bf, env)?;
                    let bv = num_val(b)?;
                    if bv == 0.0 {
                        return Err(BlissError::ArithmeticError("division by zero".into()));
                    }
                    let q = (av / bv).ceil() as i64;
                    let rem = av - (q as f64) * bv;
                    env.set_mv(vec![
                        BlissVal::from_fixnum(q),
                        BlissVal::from_single_float(rem as f32),
                    ]);
                    return Ok(BlissVal::from_fixnum(q));
                }
                return Ok(BlissVal::from_fixnum(av.ceil() as i64));
            }
            "ROUND" => {
                let (af, r) = cp(cdr);
                let a = eval_form(af, env)?;
                let av = num_val(a)?;
                if r.is_cons() {
                    let (bf, _) = cp(r);
                    let b = eval_form(bf, env)?;
                    let bv = num_val(b)?;
                    if bv == 0.0 {
                        return Err(BlissError::ArithmeticError("division by zero".into()));
                    }
                    let q = (av / bv).round() as i64;
                    let rem = av - (q as f64) * bv;
                    env.set_mv(vec![
                        BlissVal::from_fixnum(q),
                        BlissVal::from_single_float(rem as f32),
                    ]);
                    return Ok(BlissVal::from_fixnum(q));
                }
                return Ok(BlissVal::from_fixnum(av.round() as i64));
            }
            "EXPT" => {
                let (af, r) = cp(cdr);
                let (bf, _) = cp(r);
                let a = eval_form(af, env)?;
                let b = eval_form(bf, env)?;
                // Exact result when the base is rational and the exponent is an
                // integer: an exact rational/bignum, promoting past i64 range.
                if !a.is_single_float() && b.is_fixnum() {
                    if let Some(base) = as_bigrat(a) {
                        let e = b.as_fixnum();
                        if e >= 0 {
                            return Ok(bigrat_pow(&base, e as u64).to_val());
                        }
                        if base.num.is_zero() {
                            return Err(BlissError::ArithmeticError("division by zero".into()));
                        }
                        // negative exponent: reciprocal of base^|e|
                        let p = bigrat_pow(&base, e.unsigned_abs());
                        return Ok(bigrat_div(&BigRat::from_i64(1), &p).to_val());
                    }
                }
                let av = num_val(a)?;
                let bv = num_val(b)?;
                return Ok(BlissVal::from_single_float(av.powf(bv) as f32));
            }
            "SQRT" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                let nv = num_val(v)?;
                return Ok(BlissVal::from_single_float(nv.sqrt() as f32));
            }
            "NTH-VALUE" => {
                let (nf, r) = cp(cdr);
                let (form, _) = cp(r);
                let n = eval_form(nf, env)?;
                let idx = num_val(n)? as usize;
                let (_, values) = eval_form_collecting_values(form, env)?;
                return Ok(values.get(idx).copied().unwrap_or(NIL));
            }
            "MULTIPLE-VALUE-BIND" => return eval_multiple_value_bind(cdr, env),
            "MULTIPLE-VALUE-CALL" => {
                // (multiple-value-call function form*) — gather all the values
                // produced by each form into a single argument list.
                let (fn_form, forms) = cp(cdr);
                let fn_val = eval_form(fn_form, env)?;
                let mut args = Vec::new();
                let mut c = forms;
                while c.is_cons() {
                    let (af, r) = cp(c);
                    let (_, values) = eval_form_collecting_values(af, env)?;
                    args.extend(values);
                    c = r;
                }
                return apply_function(fn_val, &args, env);
            }
            "MULTIPLE-VALUE-PROG1" => {
                let (first_form, rest_forms) = cp(cdr);
                let (primary, saved_values) = eval_form_collecting_values(first_form, env)?;
                let _ = eval_progn(rest_forms, env)?;
                env.set_mv(saved_values);
                return Ok(primary);
            }
            "MULTIPLE-VALUE-SETQ" => {
                let (vars_form, rest) = cp(cdr);
                let (values_form, _) = cp(rest);
                let vars = list_to_vec(vars_form);
                let (_, values) = eval_form_collecting_values(values_form, env)?;
                for (index, var_form) in vars.iter().enumerate() {
                    let name = sym_name(*var_form);
                    env.set_var(&name, values.get(index).copied().unwrap_or(NIL));
                }
                if values.is_empty() {
                    env.set_mv(Vec::new());
                    return Ok(NIL);
                }
                env.set_mv(values.clone());
                return Ok(values[0]);
            }
            "MULTIPLE-VALUE-LIST" => {
                // (multiple-value-list form) — a list of all the values of form.
                let (form, _) = cp(cdr);
                let (_, values) = eval_form_collecting_values(form, env)?;
                return Ok(vec_to_list(&values));
            }
            "VALUES-LIST" => {
                // (values-list list) — return the elements of list as values.
                let (form, _) = cp(cdr);
                let lst = eval_form(form, env)?;
                let vals = list_to_vec(lst);
                if vals.is_empty() {
                    env.set_mv(Vec::new());
                    return Ok(NIL);
                }
                env.set_mv(vals.clone());
                return Ok(vals[0]);
            }
            "HANDLER-CASE" => return eval_handler_case(cdr, env),
            "HANDLER-BIND" => return eval_handler_bind(cdr, env),
            "RESTART-BIND" => return eval_restart_bind(cdr, env),
            "RESTART-CASE" => return eval_restart_case(cdr, env),
            "SIGNAL" => {
                let args = list_to_vec(cdr);
                if args.is_empty() {
                    return Err(BlissError::Internal("SIGNAL requires an argument".into()));
                }
                let cond = eval_form(args[0], env)?;
                return signal_condition_object(cond, env);
            }
            "MAKE-CONDITION" => {
                let args = list_to_vec(cdr);
                if args.is_empty() {
                    return Err(BlissError::Internal(
                        "MAKE-CONDITION requires a type".into(),
                    ));
                }
                let type_val = eval_form(args[0], env)?;
                let type_name = if type_val.is_symbol() {
                    sym_name(type_val)
                } else {
                    val_as_str(type_val)
                };
                let class = ensure_condition_class_registered(env, &type_name)?;
                let slot_specs = condition_slot_specs(env, &type_name);
                let mut initargs = Vec::new();
                let mut seen_initargs = Vec::new();
                let mut i = 1;
                while i + 1 < args.len() {
                    let key = eval_form(args[i], env)?;
                    let val = eval_form(args[i + 1], env)?;
                    let key_name = symbol_bare_name(&sym_name(key));
                    let slot_name = slot_specs
                        .iter()
                        .find(|(_, initarg)| initarg == &key_name)
                        .map(|(slot_name, _)| slot_name.clone())
                        .unwrap_or_else(|| key_name.clone());
                    seen_initargs.push(key_name);
                    initargs.push(resolve_sym(&slot_name).unwrap_or(NIL));
                    initargs.push(val);
                    i += 2;
                }
                for (initarg_name, default_value) in condition_default_initargs(env, &type_name) {
                    if seen_initargs.iter().any(|seen| seen == &initarg_name) {
                        continue;
                    }
                    if let Some((slot_name, _)) = slot_specs
                        .iter()
                        .find(|(_, initarg)| initarg == &initarg_name)
                    {
                        initargs.push(resolve_sym(slot_name).unwrap_or(NIL));
                        initargs.push(default_value);
                    }
                }
                return bliss_stdlib::make_instance(class, &initargs);
            }
            "SLOT-VALUE" => {
                let (instance_form, rest) = cp(cdr);
                let (slot_form, _) = cp(rest);
                let instance = eval_form(instance_form, env)?;
                let slot = eval_form(slot_form, env)?;
                return read_slot_value(instance, slot, env);
            }
            "SLOT-BOUNDP" => {
                let (instance_form, rest) = cp(cdr);
                let (slot_form, _) = cp(rest);
                let instance = eval_form(instance_form, env)?;
                let slot = eval_form(slot_form, env)?;
                return Ok(if slot_is_bound(instance, slot, env)? {
                    T
                } else {
                    NIL
                });
            }
            "CLASS-OF" => {
                let (object_form, _) = cp(cdr);
                let object = eval_form(object_form, env)?;
                return Ok(bliss_stdlib::class_of(object));
            }
            "INITIALIZE-INSTANCE" => {
                let (instance_form, init_args) = cp(cdr);
                let instance = eval_form(instance_form, env)?;
                let class_name = class_name_for_instance_class(bliss_stdlib::class_of(instance));
                let initargs = evaluated_initargs(&class_name, init_args, env)?;
                let explicit_slots: Vec<String> = initargs
                    .chunks_exact(2)
                    .map(|pair| symbol_bare_name(&sym_name(pair[0])))
                    .collect();
                let (instance_initargs, class_initargs) =
                    split_initargs_for_class(env, &class_name, &initargs);
                bliss_stdlib::initialize_instance(instance, &instance_initargs)?;
                for (slot_name, value) in class_initargs {
                    write_class_slot_value(env, &class_name, &slot_name, Some(value));
                }
                apply_class_initforms(instance, &class_name, env, None, &explicit_slots)?;
                return Ok(instance);
            }
            "REINITIALIZE-INSTANCE" => {
                let (instance_form, init_args) = cp(cdr);
                let instance = eval_form(instance_form, env)?;
                let class_name = class_name_for_instance_class(bliss_stdlib::class_of(instance));
                let initargs = evaluated_initargs(&class_name, init_args, env)?;
                let (instance_initargs, class_initargs) =
                    split_initargs_for_class(env, &class_name, &initargs);
                bliss_stdlib::reinitialize_instance(instance, &instance_initargs)?;
                for (slot_name, value) in class_initargs {
                    write_class_slot_value(env, &class_name, &slot_name, Some(value));
                }
                return Ok(instance);
            }
            "CHANGE-CLASS" => {
                let (instance_form, rest) = cp(cdr);
                let (class_form, _) = cp(rest);
                let instance = eval_form(instance_form, env)?;
                let old_class_name = class_name_for_instance_class(bliss_stdlib::class_of(instance));
                let class_input = eval_form(class_form, env)?;
                let class = resolve_class_metaobject(env, class_input)?;
                bliss_stdlib::change_class(instance, class)?;
                let new_class_name = class_name_for_instance_class(class);
                let added_slots = if let Some(class_def) = env.classes.get(&new_class_name) {
                    class_def
                        .slots
                        .iter()
                        .filter(|slot| {
                            !env.classes
                                .get(&old_class_name)
                                .map(|old| old.slots.iter().any(|s| s.name == slot.name))
                                .unwrap_or(false)
                        })
                        .map(|slot| slot.name.clone())
                        .collect::<Vec<_>>()
                } else {
                    Vec::new()
                };
                apply_class_initforms(instance, &new_class_name, env, Some(&added_slots), &[])?;
                return Ok(instance);
            }
            "CALL-NEXT-METHOD" => {
                let context = env.method_context.last().cloned().ok_or_else(|| {
                    BlissError::UndefinedFunction(resolve_sym("CALL-NEXT-METHOD").unwrap_or(NIL))
                })?;
                let args = if cdr.is_nil() {
                    context.args.clone()
                } else {
                    let mut args = Vec::new();
                    let mut cursor = cdr;
                    while cursor.is_cons() {
                        let (arg_form, rest) = cp(cursor);
                        args.push(eval_form(arg_form, env)?);
                        cursor = rest;
                    }
                    args
                };
                return invoke_next_method(env, &context, &args);
            }
            "CERROR" => return eval_cerror(cdr, env),
            "COMPUTE-RESTARTS" => {
                let args = list_to_vec(cdr);
                let condition = if args.is_empty() {
                    None
                } else {
                    Some(eval_form(args[0], env)?)
                };
                let mut restarts = Vec::new();
                for restart in env.restarts.iter().rev().cloned().collect::<Vec<_>>() {
                    if restart_applies(&restart, condition, env)? {
                        restarts.push(resolve_sym(&restart.name).unwrap_or(NIL));
                    }
                }
                return Ok(vec_to_list(&restarts));
            }
            "FIND-RESTART" => {
                let (name_form, rest) = cp(cdr);
                let restart_name = symbol_bare_name(&val_as_str(eval_form(name_form, env)?));
                let condition = if rest.is_cons() {
                    Some(eval_form(cp(rest).0, env)?)
                } else {
                    None
                };
                for restart in env.restarts.iter().rev().cloned().collect::<Vec<_>>() {
                    if symbol_bare_name(&restart.name) == restart_name
                        && restart_applies(&restart, condition, env)?
                    {
                        return Ok(resolve_sym(&restart.name).unwrap_or(NIL));
                    }
                }
                return Ok(NIL);
            }
            "INVOKE-RESTART" => {
                let (name_form, rest_args) = cp(cdr);
                let name_val = eval_form(name_form, env)?;
                let restart_name = val_as_str(name_val).to_uppercase();
                let mut args = Vec::new();
                let mut c = rest_args;
                while c.is_cons() {
                    let (arg_form, rest) = cp(c);
                    args.push(eval_form(arg_form, env)?);
                    c = rest;
                }
                for restart in env.restarts.iter().rev().cloned().collect::<Vec<_>>() {
                    if restart.name == restart_name {
                        let result = invoke_restart_function(&restart.function, &args, env)?;
                        if restart.unwind_on_invoke {
                            store_control_value(&format!("RESTART-RESULT:{restart_name}"), result);
                            return Err(BlissError::Internal(format!(
                                "__RESTART_INVOKED__:{}",
                                restart_name
                            )));
                        }
                        return Ok(result);
                    }
                }
                return Err(BlissError::Internal(format!(
                    "Restart {} not found",
                    restart_name
                )));
            }
            "INVOKE-RESTART-INTERACTIVELY" => {
                let (restart_form, _) = cp(cdr);
                let restart = eval_form(restart_form, env)?;
                let restart_name = symbol_bare_name(&val_as_str(restart)).to_uppercase();
                let Some(entry) = env
                    .restarts
                    .iter()
                    .rev()
                    .find(|entry| entry.name == restart_name)
                    .cloned()
                else {
                    return Err(BlissError::Internal(format!(
                        "Restart {} not found",
                        restart_name
                    )));
                };
                let interactive_args = if let Some(interactive) = &entry.interactive_function {
                    let value = invoke_restart_function(interactive, &[], env)?;
                    if value.is_nil() {
                        Vec::new()
                    } else if value.is_cons() {
                        list_to_vec(value)
                    } else {
                        vec![value]
                    }
                } else {
                    Vec::new()
                };
                let result = invoke_restart_function(&entry.function, &interactive_args, env)?;
                if entry.unwind_on_invoke {
                    store_control_value(&format!("RESTART-RESULT:{restart_name}"), result);
                    return Err(BlissError::Internal(format!(
                        "__RESTART_INVOKED__:{}",
                        restart_name
                    )));
                }
                return Ok(result);
            }
            "WITH-OPEN-FILE" => return eval_with_open_file(cdr, env),
            "LOAD" => {
                let (path_form, _) = cp(cdr);
                let path_val = eval_form(path_form, env)?;
                return load_path_into_env(&val_as_str(path_val), env);
            }
            "REQUIRE" => {
                let (module_form, _) = cp(cdr);
                let module_val = eval_form(module_form, env)?;
                return require_module(&val_as_str(module_val), env);
            }
            "PROVIDE" => {
                let (module_form, _) = cp(cdr);
                let module_val = eval_form(module_form, env)?;
                env.define_local("*LAST-PROVIDED-MODULE*", module_val);
                return Ok(module_val);
            }
            "BLISS-EXT:GETENV" => {
                let (name_form, _) = cp(cdr);
                let name = val_as_str(eval_form(name_form, env)?);
                return Ok(std::env::var(&name)
                    .ok()
                    .map(|value| arena_str(&value))
                    .unwrap_or(NIL));
            }
            "READ-LINE" => {
                let args = list_to_vec(cdr);
                let stream = if !args.is_empty() {
                    eval_form(args[0], env)?
                } else {
                    NIL // stdin
                };
                if is_stream(stream) {
                    // Read from file stream
                    let (line, eof) = stream_read_line(stream)?;
                    if eof && line.is_empty() {
                        return Ok(NIL);
                    }
                    env.set_mv(vec![arena_str(&line), if eof { T } else { NIL }]);
                    return Ok(arena_str(&line));
                } else {
                    // Read from stdin
                    let mut line = String::new();
                    std::io::stdin()
                        .read_line(&mut line)
                        .map_err(|e| BlissError::StreamError(format!("read-line: {}", e)))?;
                    let trimmed = line.trim_end_matches('\n').trim_end_matches('\r');
                    return Ok(arena_str(trimmed));
                }
            }
            "WRITE-STRING" => {
                let args = list_to_vec(cdr);
                if args.is_empty() {
                    return Err(BlissError::Internal(
                        "WRITE-STRING requires an argument".into(),
                    ));
                }
                let string = eval_form(args[0], env)?;
                let s = val_as_str(string);
                if args.len() > 1 {
                    let stream = eval_form(args[1], env)?;
                    if is_stream(stream) {
                        stream_write_string(stream, &s)?;
                        return Ok(string);
                    }
                }
                print!("{}", s);
                return Ok(string);
            }
            "SAVE-IMAGE" => {
                let (path_form, _) = cp(cdr);
                let path_val = eval_form(path_form, env)?;
                let path = val_as_str(path_val);
                // Save a minimal image: serialize the environment's function definitions
                let mut image_data = String::new();
                for (name, fdef) in env.funs.iter() {
                    let params_str = fdef.params.join(" ");
                    let body_str = format_body_forms(fdef.body);
                    image_data
                        .push_str(&format!("(defun {} ({}) {})\n", name, params_str, body_str));
                }
                std::fs::write(&path, &image_data)
                    .map_err(|e| BlissError::FileError(format!("save-image: {}", e)))?;
                return Ok(T);
            }
            "DEFPACKAGE" => return eval_defpackage(cdr, env),
            "IN-PACKAGE" => {
                let (pkg_form, _) = cp(cdr);
                let pkg_val = eval_form(pkg_form, env)?;
                let pkg_name = val_as_str(pkg_val)
                    .trim_start_matches("KEYWORD:")
                    .trim_start_matches(':')
                    .to_uppercase();
                env.current_package = pkg_name;
                env.define_local("*PACKAGE*", arena_str(&env.current_package));
                return Ok(T);
            }
            "MAKE-PACKAGE" => {
                let args = list_to_vec(cdr);
                if args.is_empty() {
                    return Err(BlissError::Internal(
                        "MAKE-PACKAGE requires a package designator".into(),
                    ));
                }
                let pkg_name = normalize_package_name(&val_as_str(eval_form(args[0], env)?));
                ensure_package_available(env, &pkg_name, &[]);
                return Ok(arena_str(&pkg_name));
            }
            "FIND-PACKAGE" => {
                let args = list_to_vec(cdr);
                if args.is_empty() {
                    return Ok(NIL);
                }
                let pkg_name = normalize_package_name(&val_as_str(eval_form(args[0], env)?));
                return Ok(
                    if env.packages.contains_key(&pkg_name)
                        || matches!(
                            pkg_name.as_str(),
                            "COMMON-LISP" | "COMMON-LISP-USER" | "KEYWORD"
                        )
                    {
                        arena_str(&pkg_name)
                    } else {
                        NIL
                    },
                );
            }
            "PACKAGE-NAME" => {
                let args = list_to_vec(cdr);
                if args.is_empty() {
                    return Ok(NIL);
                }
                let pkg = val_as_str(eval_form(args[0], env)?);
                return Ok(if pkg.is_empty() {
                    NIL
                } else {
                    arena_str(&normalize_package_name(&pkg))
                });
            }
            "PACKAGE-NAMES" => {
                let args = list_to_vec(cdr);
                if args.is_empty() {
                    return Ok(NIL);
                }
                let pkg = normalize_package_name(&val_as_str(eval_form(args[0], env)?));
                return Ok(vec_to_list(&[arena_str(&pkg)]));
            }
            "PACKAGE-NICKNAMES" | "PACKAGE-SHADOWING-SYMBOLS" | "PACKAGE-USED-BY-LIST" => {
                return Ok(NIL);
            }
            "PACKAGE-USE-LIST" => {
                let args = list_to_vec(cdr);
                if args.is_empty() {
                    return Ok(NIL);
                }
                let pkg_name = normalize_package_name(&val_as_str(eval_form(args[0], env)?));
                let uses = env
                    .packages
                    .get(&pkg_name)
                    .map(|pkg| {
                        pkg.uses
                            .iter()
                            .map(|name| arena_str(name))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                return Ok(vec_to_list(&uses));
            }
            "LIST-ALL-PACKAGES" => {
                let packages = env
                    .packages
                    .keys()
                    .map(|name| arena_str(name))
                    .collect::<Vec<_>>();
                return Ok(vec_to_list(&packages));
            }
            "BLISS-INTERNAL::PACKAGE-SYMBOLS" | "BLISS-INTERNAL:PACKAGE-SYMBOLS" => {
                let args = list_to_vec(cdr);
                let package = if args.is_empty() {
                    env.current_package.clone()
                } else {
                    normalize_package_name(&val_as_str(eval_form(args[0], env)?))
                };
                let include_inherited = if let Some(arg) = args.get(1) {
                    !eval_form(*arg, env)?.is_nil()
                } else {
                    false
                };
                return Ok(vec_to_list(&package_symbols(
                    env,
                    &package,
                    include_inherited,
                )));
            }
            "USE-PACKAGE" => {
                let args = list_to_vec(cdr);
                if args.is_empty() {
                    return Ok(T);
                }
                let mut names = Vec::new();
                let pkgs_val = eval_form(args[0], env)?;
                if pkgs_val.is_cons() {
                    for pkg in list_to_vec(pkgs_val) {
                        names.push(normalize_package_name(&val_as_str(pkg)));
                    }
                } else {
                    names.push(normalize_package_name(&val_as_str(pkgs_val)));
                }
                let target = if args.len() > 1 {
                    normalize_package_name(&val_as_str(eval_form(args[1], env)?))
                } else {
                    env.current_package.clone()
                };
                ensure_package_available(env, &target, &[]);
                let package = Rc::make_mut(&mut env.packages)
                    .get_mut(&target)
                    .expect("package exists");
                for name in names {
                    if !package.uses.contains(&name) {
                        package.uses.push(name);
                    }
                }
                return Ok(T);
            }
            "UNUSE-PACKAGE" => return Ok(T),
            "RENAME-PACKAGE" => {
                let args = list_to_vec(cdr);
                if args.len() < 2 {
                    return Err(BlissError::Internal(
                        "RENAME-PACKAGE requires package and new name".into(),
                    ));
                }
                let old_name = normalize_package_name(&val_as_str(eval_form(args[0], env)?));
                let new_name = normalize_package_name(&val_as_str(eval_form(args[1], env)?));
                if let Some(mut pkg) = Rc::make_mut(&mut env.packages).remove(&old_name) {
                    pkg.name = new_name.clone();
                    Rc::make_mut(&mut env.packages).insert(new_name.clone(), pkg);
                }
                reader::register_package(&new_name);
                return Ok(arena_str(&new_name));
            }
            "DELETE-PACKAGE" => {
                let args = list_to_vec(cdr);
                if args.is_empty() {
                    return Ok(T);
                }
                let pkg_name = normalize_package_name(&val_as_str(eval_form(args[0], env)?));
                Rc::make_mut(&mut env.packages).remove(&pkg_name);
                return Ok(T);
            }
            "INTERN" => {
                let (name_form, rest) = cp(cdr);
                let name_val = eval_form(name_form, env)?;
                let name_str = symbol_bare_name(&val_as_str(name_val));
                let pkg_name = if rest.is_cons() {
                    normalize_package_name(&val_as_str(eval_form(cp(rest).0, env)?))
                } else {
                    env.current_package.clone()
                };
                let sym = intern_into_package(env, &pkg_name, &name_str);
                env.set_mv(vec![
                    sym,
                    if sym.is_symbol() {
                        package_status_symbol("INTERNAL")
                    } else {
                        NIL
                    },
                ]);
                return Ok(sym);
            }
            "FIND-SYMBOL" => {
                let args = list_to_vec(cdr);
                if args.len() < 2 {
                    return Err(BlissError::Internal(
                        "FIND-SYMBOL requires a name and package".into(),
                    ));
                }
                let name = symbol_bare_name(&val_as_str(eval_form(args[0], env)?));
                let pkg_name = normalize_package_name(&val_as_str(eval_form(args[1], env)?));
                if let Some((sym, status)) = find_symbol_in_package(env, &pkg_name, &name) {
                    env.set_mv(vec![sym, package_status_symbol(status)]);
                    return Ok(sym);
                }
                env.set_mv(vec![NIL, NIL]);
                return Ok(NIL);
            }
            "EXPORT" | "IMPORT" | "SHADOWING-IMPORT" => {
                let args = list_to_vec(cdr);
                if args.is_empty() {
                    return Ok(T);
                }
                let symbols = eval_form(args[0], env)?;
                let pkg_name = if args.len() > 1 {
                    normalize_package_name(&val_as_str(eval_form(args[1], env)?))
                } else {
                    env.current_package.clone()
                };
                ensure_package_available(env, &pkg_name, &[]);
                let mut names = Vec::new();
                if symbols.is_cons() {
                    for sym in list_to_vec(symbols) {
                        names.push(symbol_bare_name(&val_as_str(sym)));
                    }
                } else {
                    names.push(symbol_bare_name(&val_as_str(symbols)));
                }
                let export_mode = car.is_symbol() && sym_name(car) == "EXPORT";
                let mut resolved = Vec::with_capacity(names.len());
                for name in names {
                    let sym = intern_into_package(env, &pkg_name, &name);
                    resolved.push((name, sym));
                }
                let package = Rc::make_mut(&mut env.packages)
                    .get_mut(&pkg_name)
                    .expect("package exists");
                for (name, sym) in resolved {
                    package.symbols.insert(name.clone(), sym);
                    if export_mode && !package.exports.contains(&name) {
                        package.exports.push(name);
                    }
                }
                return Ok(T);
            }
            "UNEXPORT" => return Ok(T),
            "SHADOW" => {
                let args = list_to_vec(cdr);
                if args.is_empty() {
                    return Ok(T);
                }
                let names_val = eval_form(args[0], env)?;
                let pkg_name = if args.len() > 1 {
                    normalize_package_name(&val_as_str(eval_form(args[1], env)?))
                } else {
                    env.current_package.clone()
                };
                ensure_package_available(env, &pkg_name, &[]);
                let mut names = Vec::new();
                if names_val.is_cons() {
                    for name in list_to_vec(names_val) {
                        names.push(symbol_bare_name(&val_as_str(name)));
                    }
                } else {
                    names.push(symbol_bare_name(&val_as_str(names_val)));
                }
                for name in names {
                    intern_into_package(env, &pkg_name, &name);
                }
                return Ok(T);
            }
            "UNINTERN" => {
                let args = list_to_vec(cdr);
                if args.is_empty() {
                    return Ok(NIL);
                }
                let symbol = eval_form(args[0], env)?;
                let pkg_name = if args.len() > 1 {
                    normalize_package_name(&val_as_str(eval_form(args[1], env)?))
                } else {
                    env.current_package.clone()
                };
                let name = symbol_bare_name(&val_as_str(symbol));
                let Some(package) = Rc::make_mut(&mut env.packages).get_mut(&pkg_name) else {
                    return Ok(NIL);
                };
                let removed_symbol = package.symbols.remove(&name).is_some();
                let removed_export =
                    if let Some(pos) = package.exports.iter().position(|n| n == &name) {
                        package.exports.remove(pos);
                        true
                    } else {
                        false
                    };
                return Ok(if removed_symbol || removed_export {
                    T
                } else {
                    NIL
                });
            }
            "SYMBOL-PACKAGE" => {
                let args = list_to_vec(cdr);
                if args.is_empty() {
                    return Ok(NIL);
                }
                let sym = eval_form(args[0], env)?;
                if !sym.is_symbol() {
                    return Ok(NIL);
                }
                let name = sym_name(sym);
                let package = if name.starts_with("KEYWORD:") {
                    Some("KEYWORD".to_string())
                } else if let Some((pkg, _)) = name.rsplit_once("::") {
                    Some(pkg.to_string())
                } else if let Some((pkg, _)) = name.rsplit_once(':') {
                    Some(pkg.to_string())
                } else {
                    Some("COMMON-LISP".to_string())
                };
                return Ok(package.map(|pkg| arena_str(&pkg)).unwrap_or(NIL));
            }
            "GENSYM" => {
                thread_local! { static COUNTER: RefCell<u64> = const { RefCell::new(0) }; }
                let n = COUNTER.with(|c| {
                    let v = *c.borrow();
                    *c.borrow_mut() = v + 1;
                    v
                });
                let name = format!("G{}", n);
                match resolve_sym(&name) {
                    Some(sym) => return Ok(sym),
                    None => return Ok(arena_str(&name)),
                }
            }
            "DOTIMES" => {
                // (dotimes (var count [result]) body...)
                let (binding, body) = cp(cdr);
                let (var_form, br) = cp(binding);
                let (count_form, result_rest) = cp(br);
                let var_name = sym_name(var_form);
                let count = eval_form(count_form, env)?;
                let n = num_val(count)? as i64;
                for i in 0..n {
                    env.define_local(&var_name, BlissVal::from_fixnum(i));
                    eval_progn(body, env)?;
                }
                if result_rest.is_cons() {
                    let (result_form, _) = cp(result_rest);
                    env.define_local(&var_name, BlissVal::from_fixnum(n));
                    return eval_form(result_form, env);
                }
                return Ok(NIL);
            }
            "DOLIST" => {
                // (dolist (var list [result]) body...)
                let (binding, body) = cp(cdr);
                let (var_form, br) = cp(binding);
                let (list_form, result_rest) = cp(br);
                let var_name = sym_name(var_form);
                let list = eval_form(list_form, env)?;
                let elems = list_to_vec(list);
                for e in &elems {
                    env.define_local(&var_name, *e);
                    eval_progn(body, env)?;
                }
                if result_rest.is_cons() {
                    let (result_form, _) = cp(result_rest);
                    env.define_local(&var_name, NIL);
                    return eval_form(result_form, env);
                }
                return Ok(NIL);
            }
            "STRING=" => {
                let (af, r) = cp(cdr);
                let (bf, _) = cp(r);
                let a = eval_form(af, env)?;
                let b = eval_form(bf, env)?;
                return Ok(if val_as_str(a) == val_as_str(b) {
                    T
                } else {
                    NIL
                });
            }
            "STRING<" => {
                let (af, r) = cp(cdr);
                let (bf, _) = cp(r);
                let a = eval_form(af, env)?;
                let b = eval_form(bf, env)?;
                return Ok(if val_as_str(a) < val_as_str(b) {
                    T
                } else {
                    NIL
                });
            }
            "STRING>" => {
                let (af, r) = cp(cdr);
                let (bf, _) = cp(r);
                let a = eval_form(af, env)?;
                let b = eval_form(bf, env)?;
                return Ok(if val_as_str(a) > val_as_str(b) {
                    T
                } else {
                    NIL
                });
            }
            "TYPE-OF" => {
                let (af, _) = cp(cdr);
                let v = eval_form(af, env)?;
                let type_name = if v.is_nil() {
                    "NULL"
                } else if v == T {
                    "BOOLEAN"
                } else if v.is_fixnum() {
                    "FIXNUM"
                } else if v.is_single_float() {
                    "SINGLE-FLOAT"
                } else if v.is_character() {
                    "CHARACTER"
                } else if v.is_symbol() {
                    "SYMBOL"
                } else if v.is_cons() {
                    "CONS"
                } else if v.is_string() {
                    "SIMPLE-BASE-STRING"
                } else {
                    "T"
                };
                match resolve_sym(type_name) {
                    Some(sym) => return Ok(sym),
                    None => return Ok(arena_str(type_name)),
                }
            }
            _ => {}
        }

        // Check user-defined functions
        if let Some(fdef) = env.funs.get(&name).cloned() {
            let mut args = Vec::new();
            let mut c = cdr;
            while c.is_cons() {
                let (af, r) = cp(c);
                args.push(eval_form(af, env)?);
                c = r;
            }
            return eval_lambda_call(
                env,
                fdef.params_form,
                fdef.body,
                &args,
                Rc::clone(&env.frame),
            );
        }

        // Check accessor functions (from DEFCLASS)
        // Collect matching accessor slot name to avoid borrow conflict
        let mut accessor_slot_name: Option<String> = None;
        for class in env.classes.values() {
            for slot in &class.slots {
                let reader_match = slot
                    .accessor
                    .as_ref()
                    .map(|acc| acc == &name)
                    .unwrap_or(false)
                    || slot.readers.iter().any(|reader| reader == &name);
                if reader_match {
                    accessor_slot_name = Some(slot.name.clone());
                }
            }
        }
        if let Some(slot_name) = accessor_slot_name {
            let (inst_form, _) = cp(cdr);
            let inst = eval_form(inst_form, env)?;
            return read_slot_value(inst, resolve_sym(&slot_name).unwrap_or(NIL), env);
        }

        // Check methods
        if env.generics.contains_key(&name) || env.methods.contains_key(&name) {
            let mut args = Vec::new();
            let mut c = cdr;
            while c.is_cons() {
                let (af, r) = cp(c);
                args.push(eval_form(af, env)?);
                c = r;
            }
            return invoke_generic_function(&name, &args, env);
        }

        // Check for package-qualified symbols (e.g., TEST-PKG:HELLO)
        if name.contains(':') {
            let parts: Vec<&str> = name.splitn(2, ':').collect();
            if parts.len() == 2 {
                let _pkg_name = parts[0];
                let fn_name = parts[1].trim_start_matches(':');
                if let Some(fdef) = env.funs.get(fn_name).cloned() {
                    let mut args = Vec::new();
                    let mut c = cdr;
                    while c.is_cons() {
                        let (af, r) = cp(c);
                        args.push(eval_form(af, env)?);
                        c = r;
                    }
                    return eval_lambda_call(
                        env,
                        fdef.params_form,
                        fdef.body,
                        &args,
                        Rc::clone(&env.frame),
                    );
                }
            }
        }
    }

    // Lambda application: ((lambda (params) body) args...)
    if car.is_cons() {
        let (lh, lr) = cp(car);
        if lh.is_symbol() && sym_name(lh) == "LAMBDA" {
            let (params_form, body_rest) = cp(lr);
            let mut args = Vec::new();
            let mut c = cdr;
            while c.is_cons() {
                let (af, r) = cp(c);
                args.push(eval_form(af, env)?);
                c = r;
            }
            return eval_lambda_call(env, params_form, body_rest, &args, Rc::clone(&env.frame));
        }
    }

    Err(BlissError::UndefinedFunction(car))
}

fn eval_progn(forms: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let mut r = NIL;
    let mut c = forms;
    while c.is_cons() {
        let (f, rest) = cp(c);
        r = eval_form(f, env)?;
        c = rest;
    }
    Ok(r)
}

// ── LOOP (extended, bootstrap subset) ─────────────────────────────
//
// Supports the clauses ASDF's load path exercises:
//   :with var = init [:and var = init]*
//   :for pat :in list        (pat may be a destructuring pattern, e.g. (k . v))
//   :for pat :on list
//   :initially form*   :finally form*   (a (return X) form ends the loop with X)
//   :when/:if/:unless test <clause> [:and <clause>]* [:else <clause>+] [:end]
//   :collect/:append expr [:into var]
//   :do form*          :return expr
// A "simple" LOOP (no leading keyword) runs its body repeatedly until a
// top-level (return X); nested returns are not detected (not needed at load
// time) and a hard iteration cap guards against runaway loops.

fn is_loop_keyword(bare: &str) -> bool {
    matches!(
        bare,
        "WITH"
            | "AND"
            | "FOR"
            | "IN"
            | "ON"
            | "BEING"
            | "THE"
            | "OF"
            | "THEN"
            | "WHEN"
            | "IF"
            | "UNLESS"
            | "ELSE"
            | "END"
            | "COLLECT"
            | "COLLECTING"
            | "APPEND"
            | "APPENDING"
            | "NCONC"
            | "NCONCING"
            | "DO"
            | "DOING"
            | "RETURN"
            | "THEREIS"
            | "INTO"
            | "FINALLY"
            | "INITIALLY"
            | "WHILE"
            | "UNTIL"
            | "REPEAT"
    )
}

#[derive(Clone)]
enum LoopClause {
    Do(Vec<BlissVal>),
    Collect(BlissVal, Option<String>),
    Append(BlissVal, Option<String>),
    Return(BlissVal),
    ThereIs(BlissVal),
    Cond {
        test: BlissVal,
        negate: bool,
        then: Vec<LoopClause>,
        els: Vec<LoopClause>,
    },
}

struct LoopParser {
    toks: Vec<BlissVal>,
    pos: usize,
}

impl LoopParser {
    fn peek(&self) -> Option<BlissVal> {
        self.toks.get(self.pos).copied()
    }
    fn advance(&mut self) -> Option<BlissVal> {
        let v = self.peek();
        if v.is_some() {
            self.pos += 1;
        }
        v
    }
    /// Bare (package-stripped, upcased) name of the next token, but only when
    /// it is a recognised LOOP keyword — otherwise None (it's an expression).
    fn peek_kw(&self) -> Option<String> {
        let v = self.peek()?;
        if !v.is_symbol() {
            return None;
        }
        let n = sym_name(v);
        let bare = n.strip_prefix("KEYWORD:").unwrap_or(&n).to_string();
        if is_loop_keyword(&bare) {
            Some(bare)
        } else {
            None
        }
    }
    fn at_kw(&self, k: &str) -> bool {
        self.peek_kw().as_deref() == Some(k)
    }
    fn read_form(&mut self) -> Result<BlissVal, BlissError> {
        self.advance()
            .ok_or_else(|| BlissError::Internal("LOOP: unexpected end of clauses".into()))
    }
    /// Read one-or-more forms up to the next LOOP keyword (for :do/:finally).
    fn read_forms(&mut self) -> Vec<BlissVal> {
        let mut forms = Vec::new();
        while self.pos < self.toks.len() && self.peek_kw().is_none() {
            forms.push(self.advance().unwrap());
        }
        forms
    }
    fn read_into(&mut self) -> Result<Option<String>, BlissError> {
        if self.at_kw("INTO") {
            self.advance();
            Ok(Some(sym_name(self.read_form()?)))
        } else {
            Ok(None)
        }
    }
    fn parse_clause(&mut self) -> Result<LoopClause, BlissError> {
        let kw = self
            .peek_kw()
            .ok_or_else(|| BlissError::Internal("LOOP: expected a clause keyword".into()))?;
        self.advance();
        match kw.as_str() {
            "COLLECT" | "COLLECTING" => {
                let e = self.read_form()?;
                let into = self.read_into()?;
                Ok(LoopClause::Collect(e, into))
            }
            "APPEND" | "APPENDING" | "NCONC" | "NCONCING" => {
                let e = self.read_form()?;
                let into = self.read_into()?;
                Ok(LoopClause::Append(e, into))
            }
            "DO" | "DOING" => Ok(LoopClause::Do(self.read_forms())),
            "RETURN" => Ok(LoopClause::Return(self.read_form()?)),
            "THEREIS" => Ok(LoopClause::ThereIs(self.read_form()?)),
            "WHEN" | "IF" => self.parse_cond(false),
            "UNLESS" => self.parse_cond(true),
            other => Err(BlissError::Internal(format!(
                "LOOP: unsupported clause `{}`",
                other
            ))),
        }
    }
    fn parse_cond(&mut self, negate: bool) -> Result<LoopClause, BlissError> {
        let test = self.read_form()?;
        let mut then = vec![self.parse_clause()?];
        while self.at_kw("AND") {
            self.advance();
            then.push(self.parse_clause()?);
        }
        let mut els = Vec::new();
        if self.at_kw("ELSE") {
            self.advance();
            els.push(self.parse_clause()?);
            while self.at_kw("AND") {
                self.advance();
                els.push(self.parse_clause()?);
            }
        }
        if self.at_kw("END") {
            self.advance();
        }
        Ok(LoopClause::Cond {
            test,
            negate,
            then,
            els,
        })
    }
}

#[derive(Default)]
struct LoopAccs {
    map: std::collections::HashMap<Option<String>, Vec<BlissVal>>,
}

impl LoopAccs {
    fn collect(&mut self, key: Option<String>, v: BlissVal) {
        self.map.entry(key).or_default().push(v);
    }
    fn append(&mut self, key: Option<String>, v: BlissVal) {
        let items = list_to_vec(v);
        self.map.entry(key).or_default().extend(items);
    }
}

/// Bind a (possibly destructuring / dotted) pattern against a value.
fn loop_bind(pattern: BlissVal, value: BlissVal, env: &mut Env) {
    if pattern.is_symbol() {
        let name = sym_name(pattern);
        if name != "NIL" {
            env.define_local(&name, value);
        }
    } else if pattern.is_cons() {
        let (pcar, pcdr) = cp(pattern);
        let (vcar, vcdr) = if value.is_cons() {
            cp(value)
        } else {
            (NIL, NIL)
        };
        loop_bind(pcar, vcar, env);
        loop_bind(pcdr, vcdr, env);
    }
}

/// Collect every `:into` accumulator name so they can be bound to NIL up
/// front — LOOP guarantees accumulators are bound even if never accumulated,
/// and :finally clauses read them.
fn loop_collect_intos(clauses: &[LoopClause], out: &mut Vec<String>) {
    for c in clauses {
        match c {
            LoopClause::Collect(_, Some(n)) | LoopClause::Append(_, Some(n)) => {
                if !out.contains(n) {
                    out.push(n.clone());
                }
            }
            LoopClause::Cond { then, els, .. } => {
                loop_collect_intos(then, out);
                loop_collect_intos(els, out);
            }
            _ => {}
        }
    }
}

fn loop_exec_clauses(
    clauses: &[LoopClause],
    env: &mut Env,
    accs: &mut LoopAccs,
    ret: &mut Option<BlissVal>,
) -> Result<(), BlissError> {
    for c in clauses {
        if ret.is_some() {
            break;
        }
        loop_exec_clause(c, env, accs, ret)?;
    }
    Ok(())
}

fn loop_exec_clause(
    c: &LoopClause,
    env: &mut Env,
    accs: &mut LoopAccs,
    ret: &mut Option<BlissVal>,
) -> Result<(), BlissError> {
    match c {
        LoopClause::Do(forms) => {
            for f in forms {
                if ret.is_some() {
                    break;
                }
                eval_form(*f, env)?;
            }
        }
        LoopClause::Return(e) => {
            *ret = Some(eval_form(*e, env)?);
        }
        LoopClause::ThereIs(e) => {
            let v = eval_form(*e, env)?;
            if !v.is_nil() {
                *ret = Some(v);
            }
        }
        LoopClause::Collect(e, into) => {
            let v = eval_form(*e, env)?;
            accs.collect(into.clone(), v);
        }
        LoopClause::Append(e, into) => {
            let v = eval_form(*e, env)?;
            accs.append(into.clone(), v);
        }
        LoopClause::Cond {
            test,
            negate,
            then,
            els,
        } => {
            let t = eval_form(*test, env)?;
            let take = !t.is_nil() ^ *negate;
            if take {
                loop_exec_clauses(then, env, accs, ret)?;
            } else {
                loop_exec_clauses(els, env, accs, ret)?;
            }
        }
    }
    Ok(())
}

/// If `form` is (RETURN x) or (RETURN-FROM nil x), return Some(x-form).
fn loop_return_target(form: BlissVal) -> Option<BlissVal> {
    if !form.is_cons() {
        return None;
    }
    let (head, rest) = cp(form);
    if !head.is_symbol() {
        return None;
    }
    match sym_name(head).as_str() {
        "RETURN" => Some(cp(rest).0),
        "RETURN-FROM" => {
            let (_blk, r) = cp(rest);
            Some(cp(r).0)
        }
        _ => None,
    }
}

/// A parsed `:for` iteration clause.
enum ForClause {
    In {
        pat: BlissVal,
        list_form: BlissVal,
    },
    On {
        pat: BlissVal,
        list_form: BlissVal,
    },
    Eq {
        pat: BlissVal,
        init: BlissVal,
        then: Option<BlissVal>,
    },
    From {
        pat: BlissVal,
        start: BlissVal,
        step: Option<BlissVal>,
        limit: Option<(LoopForLimit, BlissVal)>,
    },
    Across {
        pat: BlissVal,
        seq_form: BlissVal,
    },
    Being {
        pat: BlissVal,
        source: LoopBeingSource,
    },
}

/// Runtime cursor for a `:for` clause.
enum ForState {
    In {
        pat: BlissVal,
        items: Vec<BlissVal>,
        idx: usize,
    },
    On {
        pat: BlissVal,
        tail: BlissVal,
    },
    Eq {
        pat: BlissVal,
        init: BlissVal,
        then: Option<BlissVal>,
    },
    From {
        pat: BlissVal,
        current: BlissVal,
        step: BlissVal,
        limit: Option<(LoopForLimit, BlissVal)>,
    },
    Across {
        pat: BlissVal,
        items: Vec<BlissVal>,
        idx: usize,
    },
    Being {
        pat: BlissVal,
        items: Vec<BlissVal>,
        idx: usize,
    },
}

#[derive(Clone, Copy)]
enum LoopForLimit {
    Below,
    To,
    Upto,
    Above,
    Downto,
}

enum LoopBeingSource {
    Symbols(BlissVal),
    HashKeys(BlissVal),
    HashValues(BlissVal),
}

fn eval_loop(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let toks = list_to_vec(cdr);
    let mut lenv = env.child();

    // Simple LOOP: no leading keyword -> repeat body until a top-level return.
    let starts_with_kw = toks
        .first()
        .and_then(|v| {
            if v.is_symbol() {
                let n = sym_name(*v);
                Some(is_loop_keyword(n.strip_prefix("KEYWORD:").unwrap_or(&n)))
            } else {
                None
            }
        })
        .unwrap_or(false);
    if !starts_with_kw {
        let mut guard: u64 = 0;
        loop {
            for f in &toks {
                if let Some(tgt) = loop_return_target(*f) {
                    return eval_form(tgt, &mut lenv);
                }
                eval_form(*f, &mut lenv)?;
            }
            guard += 1;
            if guard > 10_000_000 {
                return Err(BlissError::Internal(
                    "LOOP: simple loop exceeded iteration cap".into(),
                ));
            }
        }
    }

    // Extended LOOP.
    let mut p = LoopParser { toks, pos: 0 };
    let mut with_bindings: Vec<(BlissVal, BlissVal)> = Vec::new();
    let mut for_clauses: Vec<ForClause> = Vec::new();
    let mut initially: Vec<BlissVal> = Vec::new();
    let mut finally: Vec<BlissVal> = Vec::new();
    let mut body: Vec<LoopClause> = Vec::new();

    while let Some(kw) = p.peek_kw() {
        match kw.as_str() {
            "WITH" => {
                p.advance();
                loop {
                    let var = p.read_form()?;
                    let eq = p.read_form()?;
                    if !(eq.is_symbol() && sym_name(eq) == "=") {
                        return Err(BlissError::Internal("LOOP :with expects `=`".into()));
                    }
                    let init = p.read_form()?;
                    with_bindings.push((var, init));
                    if p.at_kw("AND") {
                        p.advance();
                        continue;
                    }
                    break;
                }
            }
            "FOR" => {
                p.advance();
                let pat = p.read_form()?;
                match p.peek_kw().as_deref() {
                    Some("IN") => {
                        p.advance();
                        let list_form = p.read_form()?;
                        for_clauses.push(ForClause::In { pat, list_form });
                    }
                    Some("ON") => {
                        p.advance();
                        let list_form = p.read_form()?;
                        for_clauses.push(ForClause::On { pat, list_form });
                    }
                    Some("ACROSS") => {
                        p.advance();
                        let seq_form = p.read_form()?;
                        for_clauses.push(ForClause::Across { pat, seq_form });
                    }
                    Some("BEING") => {
                        p.advance();
                        if p.at_kw("THE") {
                            p.advance();
                        }
                        let kind = p.read_form()?;
                        let kind_name = if kind.is_symbol() {
                            sym_name(kind)
                        } else {
                            String::new()
                        };
                        let kind_bare = kind_name.strip_prefix("KEYWORD:").unwrap_or(&kind_name);
                        let source = match kind_bare {
                            "SYMBOLS" => {
                                let in_kw = p.read_form()?;
                                let in_name = if in_kw.is_symbol() {
                                    sym_name(in_kw)
                                } else {
                                    String::new()
                                };
                                if in_name.strip_prefix("KEYWORD:").unwrap_or(&in_name) != "IN" {
                                    return Err(BlissError::Internal(
                                        "LOOP :for ... :being :the :symbols expects :in".into(),
                                    ));
                                }
                                LoopBeingSource::Symbols(p.read_form()?)
                            }
                            "HASH-KEYS" => {
                                let of_kw = p.read_form()?;
                                let of_name = if of_kw.is_symbol() {
                                    sym_name(of_kw)
                                } else {
                                    String::new()
                                };
                                if of_name.strip_prefix("KEYWORD:").unwrap_or(&of_name) != "OF" {
                                    return Err(BlissError::Internal(
                                        "LOOP :for ... :being :the :hash-keys expects :of".into(),
                                    ));
                                }
                                LoopBeingSource::HashKeys(p.read_form()?)
                            }
                            "HASH-VALUES" => {
                                let of_kw = p.read_form()?;
                                let of_name = if of_kw.is_symbol() {
                                    sym_name(of_kw)
                                } else {
                                    String::new()
                                };
                                if of_name.strip_prefix("KEYWORD:").unwrap_or(&of_name) != "OF" {
                                    return Err(BlissError::Internal(
                                        "LOOP :for ... :being :the :hash-values expects :of".into(),
                                    ));
                                }
                                LoopBeingSource::HashValues(p.read_form()?)
                            }
                            _ => {
                                return Err(BlissError::Internal(
                                    "LOOP :for ... :being supports :symbols / :hash-keys / :hash-values in the bootstrap"
                                        .into(),
                                ));
                            }
                        };
                        for_clauses.push(ForClause::Being { pat, source });
                    }
                    Some("FROM") => {
                        p.advance();
                        let start = p.read_form()?;
                        let mut step = None;
                        let mut limit = None;
                        loop {
                            match p.peek_kw().as_deref() {
                                Some("BY") => {
                                    p.advance();
                                    step = Some(p.read_form()?);
                                }
                                Some("BELOW") => {
                                    p.advance();
                                    limit = Some((LoopForLimit::Below, p.read_form()?));
                                }
                                Some("TO") => {
                                    p.advance();
                                    limit = Some((LoopForLimit::To, p.read_form()?));
                                }
                                Some("UPTO") => {
                                    p.advance();
                                    limit = Some((LoopForLimit::Upto, p.read_form()?));
                                }
                                Some("ABOVE") => {
                                    p.advance();
                                    limit = Some((LoopForLimit::Above, p.read_form()?));
                                }
                                Some("DOWNTO") => {
                                    p.advance();
                                    limit = Some((LoopForLimit::Downto, p.read_form()?));
                                }
                                _ => break,
                            }
                        }
                        for_clauses.push(ForClause::From {
                            pat,
                            start,
                            step,
                            limit,
                        });
                    }
                    _ => {
                        // :for var = init [:then step]
                        let eq = p.read_form()?;
                        if !(eq.is_symbol() && sym_name(eq) == "=") {
                            let found = if eq.is_symbol() {
                                sym_name(eq)
                            } else {
                                format!("{:?}", eq)
                            };
                            return Err(BlissError::Internal(format!(
                                "LOOP :for supports :in / :on / :across / :being / :from / = in the bootstrap (got {})",
                                found
                            )));
                        }
                        let init = p.read_form()?;
                        let then = if p.at_kw("THEN") {
                            p.advance();
                            Some(p.read_form()?)
                        } else {
                            None
                        };
                        for_clauses.push(ForClause::Eq { pat, init, then });
                    }
                }
            }
            "INITIALLY" => {
                p.advance();
                initially = p.read_forms();
            }
            "FINALLY" => {
                p.advance();
                finally = p.read_forms();
            }
            _ => body.push(p.parse_clause()?),
        }
    }

    // Establish :with bindings (sequential, LET*-style).
    for (var, init) in &with_bindings {
        let v = eval_form(*init, &mut lenv)?;
        loop_bind(*var, v, &mut lenv);
    }

    // Bind all :into accumulators to NIL up front.
    let mut into_names = Vec::new();
    loop_collect_intos(&body, &mut into_names);
    for n in &into_names {
        lenv.define_local(n, NIL);
    }

    let mut accs = LoopAccs::default();
    let mut ret: Option<BlissVal> = None;

    for f in &initially {
        eval_form(*f, &mut lenv)?;
    }

    if for_clauses.is_empty() {
        // No :for driver: run body once (covers when/collect-only loops).
        loop_exec_clauses(&body, &mut lenv, &mut accs, &mut ret)?;
    } else {
        // Build cursors, evaluating each list form once.
        let mut states: Vec<ForState> = Vec::with_capacity(for_clauses.len());
        let mut has_stepping_driver = false;
        for fc in &for_clauses {
            match fc {
                ForClause::In { pat, list_form } => {
                    let list = eval_form(*list_form, &mut lenv)?;
                    states.push(ForState::In {
                        pat: *pat,
                        items: list_to_vec(list),
                        idx: 0,
                    });
                    has_stepping_driver = true;
                }
                ForClause::On { pat, list_form } => {
                    let list = eval_form(*list_form, &mut lenv)?;
                    states.push(ForState::On {
                        pat: *pat,
                        tail: list,
                    });
                    has_stepping_driver = true;
                }
                ForClause::Eq { pat, init, then } => states.push(ForState::Eq {
                    pat: *pat,
                    init: *init,
                    then: *then,
                }),
                ForClause::From {
                    pat,
                    start,
                    step,
                    limit,
                } => {
                    let current = eval_form(*start, &mut lenv)?;
                    let step = match step {
                        Some(expr) => eval_form(*expr, &mut lenv)?,
                        None => BlissVal::from_fixnum(1),
                    };
                    let limit = match limit {
                        Some((kind, expr)) => Some((*kind, eval_form(*expr, &mut lenv)?)),
                        None => None,
                    };
                    states.push(ForState::From {
                        pat: *pat,
                        current,
                        step,
                        limit,
                    });
                    has_stepping_driver = true;
                }
                ForClause::Across { pat, seq_form } => {
                    let seq = eval_form(*seq_form, &mut lenv)?;
                    let items = if seq.is_cons() || seq.is_nil() {
                        list_to_vec(seq)
                    } else {
                        val_as_str(seq)
                            .chars()
                            .map(BlissVal::from_char)
                            .collect::<Vec<_>>()
                    };
                    states.push(ForState::Across {
                        pat: *pat,
                        items,
                        idx: 0,
                    });
                    has_stepping_driver = true;
                }
                ForClause::Being { pat, source } => {
                    let items = match source {
                        LoopBeingSource::Symbols(pkg_form) => {
                            let _ = eval_form(*pkg_form, &mut lenv)?;
                            Vec::new()
                        }
                        LoopBeingSource::HashKeys(table_form) => {
                            let table = eval_form(*table_form, &mut lenv)?;
                            bliss_stdlib::hash_table_entries(table)?
                                .into_iter()
                                .map(|(key, _)| key)
                                .collect()
                        }
                        LoopBeingSource::HashValues(table_form) => {
                            let table = eval_form(*table_form, &mut lenv)?;
                            bliss_stdlib::hash_table_entries(table)?
                                .into_iter()
                                .map(|(_, value)| value)
                                .collect()
                        }
                    };
                    states.push(ForState::Being {
                        pat: *pat,
                        items,
                        idx: 0,
                    });
                    has_stepping_driver = true;
                }
            }
        }
        let mut first = true;
        let mut guard: u64 = 0;
        loop {
            if ret.is_some() {
                break;
            }
            // Step every driver in order; later clauses see earlier bindings.
            let mut exhausted = false;
            for st in &mut states {
                match st {
                    ForState::In { pat, items, idx } => {
                        if *idx >= items.len() {
                            exhausted = true;
                            break;
                        }
                        loop_bind(*pat, items[*idx], &mut lenv);
                        *idx += 1;
                    }
                    ForState::On { pat, tail } => {
                        if !tail.is_cons() {
                            exhausted = true;
                            break;
                        }
                        loop_bind(*pat, *tail, &mut lenv);
                        *tail = cp(*tail).1;
                    }
                    ForState::Eq { pat, init, then } => {
                        let f = if first { *init } else { then.unwrap_or(*init) };
                        let v = eval_form(f, &mut lenv)?;
                        loop_bind(*pat, v, &mut lenv);
                    }
                    ForState::From {
                        pat,
                        current,
                        step,
                        limit,
                    } => {
                        if loop_from_exhausted(*current, limit.as_ref())? {
                            exhausted = true;
                            break;
                        }
                        loop_bind(*pat, *current, &mut lenv);
                        *current = loop_add_numbers(*current, *step)?;
                    }
                    ForState::Across { pat, items, idx } => {
                        if *idx >= items.len() {
                            exhausted = true;
                            break;
                        }
                        loop_bind(*pat, items[*idx], &mut lenv);
                        *idx += 1;
                    }
                    ForState::Being { pat, items, idx } => {
                        if *idx >= items.len() {
                            exhausted = true;
                            break;
                        }
                        loop_bind(*pat, items[*idx], &mut lenv);
                        *idx += 1;
                    }
                }
            }
            if exhausted {
                break;
            }
            loop_exec_clauses(&body, &mut lenv, &mut accs, &mut ret)?;
            first = false;
            if !has_stepping_driver {
                guard += 1;
                if guard > 10_000_000 {
                    return Err(BlissError::Internal(
                        "LOOP: exceeded iteration cap (no terminating driver)".into(),
                    ));
                }
            }
        }
    }

    // Publish named accumulators so :finally can read them.
    let named: Vec<(String, Vec<BlissVal>)> = accs
        .map
        .iter()
        .filter_map(|(k, v)| k.as_ref().map(|name| (name.clone(), v.clone())))
        .collect();
    for (name, items) in named {
        lenv.define_local(&name, vec_to_list(&items));
    }

    // :finally — an embedded (return X) ends the loop with X.
    for f in &finally {
        if ret.is_some() {
            break;
        }
        if let Some(tgt) = loop_return_target(*f) {
            ret = Some(eval_form(tgt, &mut lenv)?);
        } else {
            eval_form(*f, &mut lenv)?;
        }
    }

    if let Some(r) = ret {
        return Ok(r);
    }
    // Default: the anonymous accumulator's list, else NIL.
    if let Some(items) = accs.map.get(&None) {
        return Ok(vec_to_list(items));
    }
    Ok(NIL)
}

fn loop_add_numbers(lhs: BlissVal, rhs: BlissVal) -> Result<BlissVal, BlissError> {
    if lhs.is_fixnum() && rhs.is_fixnum() {
        return Ok(BlissVal::from_fixnum(lhs.as_fixnum() + rhs.as_fixnum()));
    }
    let sum = num_val(lhs)? + num_val(rhs)?;
    Ok(BlissVal::from_single_float(sum as f32))
}

fn loop_from_exhausted(
    current: BlissVal,
    limit: Option<&(LoopForLimit, BlissVal)>,
) -> Result<bool, BlissError> {
    let Some((kind, limit)) = limit else {
        return Ok(false);
    };
    let current = num_val(current)?;
    let limit = num_val(*limit)?;
    Ok(match kind {
        LoopForLimit::Below => current >= limit,
        LoopForLimit::To | LoopForLimit::Upto => current > limit,
        LoopForLimit::Above => current <= limit,
        LoopForLimit::Downto => current < limit,
    })
}

// ── Exact numeric tower: fixnum <-> bignum <-> ratio ──────────────
//
// Integer arithmetic promotes to arbitrary precision (BIGNUM, §1.8.1) on
// i64 overflow rather than silently wrapping. Exact rationals are carried as
// a pair of BigInts (denominator kept positive). Floats remain inexact and
// contaminate any operation they take part in, per CL contagion rules.

/// The 61-bit fixnum range (values outside it become BIGNUMs), matching the
/// reader's `fits_fixnum`.
const FIXNUM_MAX: i64 = (1 << 60) - 1;
const FIXNUM_MIN: i64 = -(1 << 60);

/// Allocate a RATIO heap object (numerator/denominator are integers).
fn alloc_ratio_cli(num: BlissVal, den: BlissVal) -> BlissVal {
    let data = Box::leak(Box::new(RatioData {
        header: ObjectHeader::new(type_id::RATIO, 3),
        numerator: num,
        denominator: den,
    }));
    unsafe { BlissVal::from_heap_ptr(data as *mut RatioData as *mut u8) }
}

/// Allocate a BIGNUM heap object (§1.8.1): header + sign + n_limbs + limbs.
/// Layout mirrors the reader so `print_val` renders it correctly.
fn alloc_bignum_cli(sign: i32, limbs: &[u64]) -> BlissVal {
    let n = limbs.len();
    let total_size = 16 + n * 8;
    let layout = std::alloc::Layout::from_size_align(total_size, 8).unwrap();
    unsafe {
        let ptr = std::alloc::alloc_zeroed(layout);
        *(ptr as *mut ObjectHeader) =
            ObjectHeader::new(type_id::BIGNUM, total_size.div_ceil(8) as u16);
        *(ptr.add(8) as *mut i32) = sign;
        *(ptr.add(12) as *mut u32) = n as u32;
        for (idx, &limb) in limbs.iter().enumerate() {
            *(ptr.add(16 + idx * 8) as *mut u64) = limb;
        }
        BlissVal::from_heap_ptr(ptr)
    }
}

/// Extract (numerator, denominator) BlissVals from a RATIO heap object.
fn ratio_parts_val(v: BlissVal) -> Option<(BlissVal, BlissVal)> {
    if !v.is_heap_object() {
        return None;
    }
    unsafe {
        let ptr = v.as_ptr();
        let hdr = *(ptr as *const ObjectHeader);
        if hdr.type_id() != type_id::RATIO {
            return None;
        }
        let num = *(ptr.add(8) as *const BlissVal);
        let den = *(ptr.add(16) as *const BlissVal);
        Some((num, den))
    }
}

// ── Arbitrary-precision integers (little-endian base-2^64 magnitude) ──

fn mag_trim(mut m: Vec<u64>) -> Vec<u64> {
    while !m.is_empty() && *m.last().unwrap() == 0 {
        m.pop();
    }
    m
}

fn mag_cmp(a: &[u64], b: &[u64]) -> Ordering {
    if a.len() != b.len() {
        return a.len().cmp(&b.len());
    }
    for i in (0..a.len()).rev() {
        if a[i] != b[i] {
            return a[i].cmp(&b[i]);
        }
    }
    Ordering::Equal
}

fn mag_add(a: &[u64], b: &[u64]) -> Vec<u64> {
    let mut out = Vec::with_capacity(a.len().max(b.len()) + 1);
    let mut carry: u128 = 0;
    for i in 0..a.len().max(b.len()) {
        let av = *a.get(i).unwrap_or(&0) as u128;
        let bv = *b.get(i).unwrap_or(&0) as u128;
        let s = av + bv + carry;
        out.push(s as u64);
        carry = s >> 64;
    }
    if carry != 0 {
        out.push(carry as u64);
    }
    mag_trim(out)
}

/// `a - b`, requiring `a >= b`.
fn mag_sub(a: &[u64], b: &[u64]) -> Vec<u64> {
    let mut out = Vec::with_capacity(a.len());
    let mut borrow: i128 = 0;
    for (i, &ai) in a.iter().enumerate() {
        let av = ai as i128;
        let bv = *b.get(i).unwrap_or(&0) as i128;
        let mut d = av - bv - borrow;
        if d < 0 {
            d += 1i128 << 64;
            borrow = 1;
        } else {
            borrow = 0;
        }
        out.push(d as u64);
    }
    mag_trim(out)
}

fn mag_mul(a: &[u64], b: &[u64]) -> Vec<u64> {
    if a.is_empty() || b.is_empty() {
        return Vec::new();
    }
    let mut out = vec![0u64; a.len() + b.len()];
    for (i, &av) in a.iter().enumerate() {
        let av = av as u128;
        let mut carry: u128 = 0;
        for (j, &bv) in b.iter().enumerate() {
            let cur = out[i + j] as u128 + av * bv as u128 + carry;
            out[i + j] = cur as u64;
            carry = cur >> 64;
        }
        let mut k = i + b.len();
        while carry != 0 {
            let cur = out[k] as u128 + carry;
            out[k] = cur as u64;
            carry = cur >> 64;
            k += 1;
        }
    }
    mag_trim(out)
}

fn mag_bit(m: &[u64], idx: usize) -> u64 {
    (m[idx / 64] >> (idx % 64)) & 1
}

fn mag_bitlen(m: &[u64]) -> usize {
    if m.is_empty() {
        return 0;
    }
    let top = m.len() - 1;
    64 * top + (64 - m[top].leading_zeros() as usize)
}

fn mag_shl1(m: &[u64]) -> Vec<u64> {
    let mut out = Vec::with_capacity(m.len() + 1);
    let mut carry = 0u64;
    for &x in m {
        out.push((x << 1) | carry);
        carry = x >> 63;
    }
    if carry != 0 {
        out.push(carry);
    }
    mag_trim(out)
}

fn mag_set_bit(m: &mut Vec<u64>, idx: usize) {
    let w = idx / 64;
    while m.len() <= w {
        m.push(0);
    }
    m[w] |= 1u64 << (idx % 64);
}

/// Truncating division of magnitudes: returns (quotient, remainder). `b` must
/// be nonzero. Bit-by-bit long division — O(bits · limbs), fine at test sizes.
fn mag_divmod(a: &[u64], b: &[u64]) -> (Vec<u64>, Vec<u64>) {
    if mag_cmp(a, b) == Ordering::Less {
        return (Vec::new(), a.to_vec());
    }
    let n = mag_bitlen(a);
    let mut rem: Vec<u64> = Vec::new();
    let mut quot: Vec<u64> = Vec::new();
    for i in (0..n).rev() {
        rem = mag_shl1(&rem);
        if mag_bit(a, i) == 1 {
            if rem.is_empty() {
                rem.push(1);
            } else {
                rem[0] |= 1;
            }
        }
        if mag_cmp(&rem, b) != Ordering::Less {
            rem = mag_sub(&rem, b);
            mag_set_bit(&mut quot, i);
        }
    }
    (mag_trim(quot), mag_trim(rem))
}

#[derive(Clone, PartialEq, Eq)]
struct BigInt {
    sign: i8,      // -1, 0, or 1
    mag: Vec<u64>, // little-endian; no trailing zero limbs; empty iff sign == 0
}

impl BigInt {
    fn zero() -> BigInt {
        BigInt {
            sign: 0,
            mag: Vec::new(),
        }
    }

    fn one() -> BigInt {
        BigInt {
            sign: 1,
            mag: vec![1],
        }
    }

    fn from_i64(n: i64) -> BigInt {
        if n == 0 {
            return BigInt::zero();
        }
        BigInt {
            sign: if n < 0 { -1 } else { 1 },
            mag: vec![n.unsigned_abs()],
        }
    }

    fn from_mag(sign: i8, mag: Vec<u64>) -> BigInt {
        let mag = mag_trim(mag);
        if mag.is_empty() {
            BigInt::zero()
        } else {
            BigInt {
                sign: if sign < 0 { -1 } else { 1 },
                mag,
            }
        }
    }

    fn from_parts(sign: i32, limbs: &[u64]) -> BigInt {
        BigInt::from_mag(if sign < 0 { -1 } else { 1 }, limbs.to_vec())
    }

    fn is_zero(&self) -> bool {
        self.sign == 0
    }

    fn to_f64(&self) -> f64 {
        let mut f = 0.0f64;
        for &limb in self.mag.iter().rev() {
            f = f * 18446744073709551616.0 + limb as f64; // * 2^64
        }
        if self.sign < 0 { -f } else { f }
    }

    /// Canonicalize: a fixnum when it fits the 61-bit range, else a BIGNUM.
    fn to_val(&self) -> BlissVal {
        if self.is_zero() {
            return BlissVal::from_fixnum(0);
        }
        if self.mag.len() == 1 {
            let m = self.mag[0];
            if self.sign > 0 {
                if m <= FIXNUM_MAX as u64 {
                    return BlissVal::from_fixnum(m as i64);
                }
            } else if m <= FIXNUM_MIN.unsigned_abs() {
                return BlissVal::from_fixnum(-(m as i64));
            }
        }
        alloc_bignum_cli(self.sign as i32, &self.mag)
    }
}

fn big_neg(a: &BigInt) -> BigInt {
    BigInt {
        sign: -a.sign,
        mag: a.mag.clone(),
    }
}

fn big_add(a: &BigInt, b: &BigInt) -> BigInt {
    if a.is_zero() {
        return b.clone();
    }
    if b.is_zero() {
        return a.clone();
    }
    if a.sign == b.sign {
        BigInt::from_mag(a.sign, mag_add(&a.mag, &b.mag))
    } else {
        match mag_cmp(&a.mag, &b.mag) {
            Ordering::Equal => BigInt::zero(),
            Ordering::Greater => BigInt::from_mag(a.sign, mag_sub(&a.mag, &b.mag)),
            Ordering::Less => BigInt::from_mag(b.sign, mag_sub(&b.mag, &a.mag)),
        }
    }
}

fn big_sub(a: &BigInt, b: &BigInt) -> BigInt {
    big_add(a, &big_neg(b))
}

fn big_mul(a: &BigInt, b: &BigInt) -> BigInt {
    if a.is_zero() || b.is_zero() {
        return BigInt::zero();
    }
    BigInt::from_mag(a.sign * b.sign, mag_mul(&a.mag, &b.mag))
}

fn big_cmp(a: &BigInt, b: &BigInt) -> Ordering {
    if a.sign != b.sign {
        return a.sign.cmp(&b.sign);
    }
    match a.sign {
        0 => Ordering::Equal,
        1 => mag_cmp(&a.mag, &b.mag),
        _ => mag_cmp(&b.mag, &a.mag),
    }
}

/// Truncating (toward zero) division: (quotient, remainder). `b` nonzero.
fn big_divmod(a: &BigInt, b: &BigInt) -> (BigInt, BigInt) {
    let (q, r) = mag_divmod(&a.mag, &b.mag);
    (
        BigInt::from_mag(a.sign * b.sign, q),
        BigInt::from_mag(a.sign, r), // remainder takes the dividend's sign
    )
}

/// Exact division, assuming `g` divides `a` with no remainder.
fn big_divexact(a: &BigInt, g: &BigInt) -> BigInt {
    big_divmod(a, g).0
}

/// Non-negative gcd of two integers (gcd(0,0) == 0).
fn big_gcd(x: &BigInt, y: &BigInt) -> BigInt {
    let mut a = BigInt::from_mag(1, x.mag.clone());
    let mut b = BigInt::from_mag(1, y.mag.clone());
    while !b.is_zero() {
        let (_, r) = big_divmod(&a, &b);
        a = b;
        b = BigInt::from_mag(1, r.mag);
    }
    a
}

/// Read a fixnum or BIGNUM into a BigInt. `None` for any other value.
fn bigint_from_val(v: BlissVal) -> Option<BigInt> {
    if v.is_fixnum() {
        return Some(BigInt::from_i64(v.as_fixnum()));
    }
    if v.is_heap_object() {
        unsafe {
            let ptr = v.as_ptr();
            let hdr = *(ptr as *const ObjectHeader);
            if hdr.type_id() == type_id::BIGNUM {
                let sign = *(ptr.add(8) as *const i32);
                let n = *(ptr.add(12) as *const u32) as usize;
                let mut limbs = Vec::with_capacity(n);
                for i in 0..n {
                    limbs.push(*(ptr.add(16 + i * 8) as *const u64));
                }
                return Some(BigInt::from_parts(sign, &limbs));
            }
        }
    }
    None
}

/// An exact rational: denominator kept positive and reduced to lowest terms.
#[derive(Clone)]
struct BigRat {
    num: BigInt,
    den: BigInt,
}

impl BigRat {
    fn from_i64(n: i64) -> BigRat {
        BigRat {
            num: BigInt::from_i64(n),
            den: BigInt::one(),
        }
    }

    fn from_bigint(n: BigInt) -> BigRat {
        BigRat {
            num: n,
            den: BigInt::one(),
        }
    }

    /// Build a reduced rational; `den` must be nonzero.
    fn new(mut num: BigInt, mut den: BigInt) -> BigRat {
        if den.sign < 0 {
            num = big_neg(&num);
            den = big_neg(&den);
        }
        if num.is_zero() {
            return BigRat {
                num: BigInt::zero(),
                den: BigInt::one(),
            };
        }
        let g = big_gcd(&num, &den);
        if big_cmp(&g, &BigInt::one()) != Ordering::Equal {
            num = big_divexact(&num, &g);
            den = big_divexact(&den, &g);
        }
        BigRat { num, den }
    }

    fn is_integer(&self) -> bool {
        big_cmp(&self.den, &BigInt::one()) == Ordering::Equal
    }

    fn to_f64(&self) -> f64 {
        self.num.to_f64() / self.den.to_f64()
    }

    fn to_val(&self) -> BlissVal {
        if self.is_integer() {
            self.num.to_val()
        } else {
            alloc_ratio_cli(self.num.to_val(), self.den.to_val())
        }
    }
}

fn bigrat_neg(a: &BigRat) -> BigRat {
    BigRat {
        num: big_neg(&a.num),
        den: a.den.clone(),
    }
}

fn bigrat_add(a: &BigRat, b: &BigRat) -> BigRat {
    let num = big_add(&big_mul(&a.num, &b.den), &big_mul(&b.num, &a.den));
    let den = big_mul(&a.den, &b.den);
    BigRat::new(num, den)
}

fn bigrat_sub(a: &BigRat, b: &BigRat) -> BigRat {
    let num = big_sub(&big_mul(&a.num, &b.den), &big_mul(&b.num, &a.den));
    let den = big_mul(&a.den, &b.den);
    BigRat::new(num, den)
}

fn bigrat_mul(a: &BigRat, b: &BigRat) -> BigRat {
    BigRat::new(big_mul(&a.num, &b.num), big_mul(&a.den, &b.den))
}

/// `a / b`; `b` must be nonzero (the caller checks for zero divisors).
fn bigrat_div(a: &BigRat, b: &BigRat) -> BigRat {
    BigRat::new(big_mul(&a.num, &b.den), big_mul(&a.den, &b.num))
}

fn bigrat_cmp(a: &BigRat, b: &BigRat) -> Ordering {
    // Denominators are positive, so comparing cross-products is order-preserving.
    big_cmp(&big_mul(&a.num, &b.den), &big_mul(&b.num, &a.den))
}

/// Raise an exact rational to a non-negative integer power.
fn bigrat_pow(base: &BigRat, mut e: u64) -> BigRat {
    let mut result = BigRat::from_i64(1);
    let mut b = base.clone();
    while e > 0 {
        if e & 1 == 1 {
            result = bigrat_mul(&result, &b);
        }
        e >>= 1;
        if e > 0 {
            b = bigrat_mul(&b, &b);
        }
    }
    result
}

/// A fixnum, BIGNUM, or RATIO as an exact rational; `None` otherwise.
fn as_bigrat(v: BlissVal) -> Option<BigRat> {
    if let Some(n) = bigint_from_val(v) {
        return Some(BigRat::from_bigint(n));
    }
    if let Some((nv, dv)) = ratio_parts_val(v) {
        let n = bigint_from_val(nv)?;
        let d = bigint_from_val(dv)?;
        if d.is_zero() {
            return None;
        }
        return Some(BigRat::new(n, d));
    }
    None
}

fn num_val(v: BlissVal) -> Result<f64, BlissError> {
    if v.is_single_float() {
        Ok(v.as_single_float() as f64)
    } else if let Some(r) = as_bigrat(v) {
        Ok(r.to_f64())
    } else {
        Err(BlissError::TypeError {
            datum: v,
            expected: "number".into(),
        })
    }
}

/// Exact-where-possible numeric comparison. Floats force inexact comparison
/// (CL contagion); otherwise operands compare as exact rationals.
fn numeric_cmp(a: BlissVal, b: BlissVal) -> Result<Ordering, BlissError> {
    if a.is_single_float() || b.is_single_float() {
        let av = num_val(a)?;
        let bv = num_val(b)?;
        return Ok(av.partial_cmp(&bv).unwrap_or(Ordering::Equal));
    }
    match (as_bigrat(a), as_bigrat(b)) {
        (Some(ra), Some(rb)) => Ok(bigrat_cmp(&ra, &rb)),
        (None, _) => Err(BlissError::TypeError {
            datum: a,
            expected: "number".into(),
        }),
        (_, None) => Err(BlissError::TypeError {
            datum: b,
            expected: "number".into(),
        }),
    }
}

fn eval_arith(
    args: BlissVal,
    env: &mut Env,
    init_i: i64,
    init_f: f64,
    op_f: fn(f64, f64) -> f64,
    op_r: fn(&BigRat, &BigRat) -> BigRat,
) -> Result<BlissVal, BlissError> {
    let mut acc = BigRat::from_i64(init_i);
    let mut acc_f = init_f;
    let mut is_float = false;
    let mut c = args;
    while c.is_cons() {
        let (af, r) = cp(c);
        let v = eval_form(af, env)?;
        if v.is_single_float() {
            if !is_float {
                is_float = true;
                acc_f = acc.to_f64();
            }
            acc_f = op_f(acc_f, v.as_single_float() as f64);
        } else if let Some(rv) = as_bigrat(v) {
            if is_float {
                acc_f = op_f(acc_f, rv.to_f64());
            } else {
                acc = op_r(&acc, &rv);
            }
        } else {
            return Err(BlissError::TypeError {
                datum: v,
                expected: "number".into(),
            });
        }
        c = r;
    }
    Ok(if is_float {
        BlissVal::from_single_float(acc_f as f32)
    } else {
        acc.to_val()
    })
}

fn eval_arith_sub(args: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let mut vals = Vec::new();
    let mut c = args;
    while c.is_cons() {
        let (af, r) = cp(c);
        vals.push(eval_form(af, env)?);
        c = r;
    }
    if vals.is_empty() {
        return Ok(BlissVal::from_fixnum(0));
    }
    if vals.len() == 1 {
        if vals[0].is_single_float() {
            return Ok(BlissVal::from_single_float(-vals[0].as_single_float()));
        }
        if let Some(r) = as_bigrat(vals[0]) {
            return Ok(bigrat_neg(&r).to_val());
        }
        return Err(BlissError::TypeError {
            datum: vals[0],
            expected: "number".into(),
        });
    }
    let mut is_float = vals[0].is_single_float();
    let mut acc_f = num_val(vals[0])?;
    let mut acc = as_bigrat(vals[0]).unwrap_or_else(|| BigRat::from_i64(0));
    for v in &vals[1..] {
        if v.is_single_float() {
            if !is_float {
                is_float = true;
                acc_f = acc.to_f64();
            }
            acc_f -= v.as_single_float() as f64;
        } else if let Some(rv) = as_bigrat(*v) {
            if is_float {
                acc_f -= rv.to_f64();
            } else {
                acc = bigrat_sub(&acc, &rv);
            }
        } else {
            return Err(BlissError::TypeError {
                datum: *v,
                expected: "number".into(),
            });
        }
    }
    Ok(if is_float {
        BlissVal::from_single_float(acc_f as f32)
    } else {
        acc.to_val()
    })
}

fn eval_arith_div(args: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let mut vals = Vec::new();
    let mut c = args;
    while c.is_cons() {
        let (af, r) = cp(c);
        vals.push(eval_form(af, env)?);
        c = r;
    }
    if vals.is_empty() {
        return Err(BlissError::ArithmeticError(
            "/ requires at least one argument".into(),
        ));
    }
    if vals.len() == 1 {
        if vals[0].is_single_float() {
            let v = vals[0].as_single_float();
            if v == 0.0 {
                return Err(BlissError::ArithmeticError("division by zero".into()));
            }
            return Ok(BlissVal::from_single_float(1.0 / v));
        }
        if let Some(r) = as_bigrat(vals[0]) {
            if r.num.is_zero() {
                return Err(BlissError::ArithmeticError("division by zero".into()));
            }
            // reciprocal: den/num
            return Ok(bigrat_div(&BigRat::from_i64(1), &r).to_val());
        }
        return Err(BlissError::TypeError {
            datum: vals[0],
            expected: "number".into(),
        });
    }
    let mut is_float = vals[0].is_single_float();
    let mut acc_f = num_val(vals[0])?;
    let mut acc = as_bigrat(vals[0]).unwrap_or_else(|| BigRat::from_i64(0));
    for v in &vals[1..] {
        if v.is_single_float() {
            let dv = v.as_single_float() as f64;
            if dv == 0.0 {
                return Err(BlissError::ArithmeticError("division by zero".into()));
            }
            if !is_float {
                is_float = true;
                acc_f = acc.to_f64();
            }
            acc_f /= dv;
        } else if let Some(rv) = as_bigrat(*v) {
            if rv.num.is_zero() {
                return Err(BlissError::ArithmeticError("division by zero".into()));
            }
            if is_float {
                acc_f /= rv.to_f64();
            } else {
                acc = bigrat_div(&acc, &rv);
            }
        } else {
            return Err(BlissError::TypeError {
                datum: *v,
                expected: "number".into(),
            });
        }
    }
    if is_float {
        Ok(BlissVal::from_single_float(acc_f as f32))
    } else {
        Ok(acc.to_val())
    }
}

fn eval_cmp(
    args: BlissVal,
    env: &mut Env,
    pred: fn(Ordering) -> bool,
) -> Result<BlissVal, BlissError> {
    let (af, r) = cp(args);
    let (bf, _) = cp(r);
    let a = eval_form(af, env)?;
    let b = eval_form(bf, env)?;
    Ok(if pred(numeric_cmp(a, b)?) { T } else { NIL })
}

fn eval_args(args: BlissVal, env: &mut Env) -> Result<Vec<BlissVal>, BlissError> {
    let mut result = Vec::new();
    let mut c = args;
    while c.is_cons() {
        let (af, r) = cp(c);
        result.push(eval_form(af, env)?);
        c = r;
    }
    Ok(result)
}

// ── LET binding (issue #2 fix) ───────────────────────────────────
fn eval_let(cdr: BlissVal, env: &mut Env, sequential: bool) -> Result<BlissVal, BlissError> {
    let (bindings_form, body) = cp(cdr);
    if sequential {
        let mut child_env = env.child();
        let mut c = bindings_form;
        while c.is_cons() {
            let (binding, rest) = cp(c);
            if binding.is_cons() {
                let (var_form, val_rest) = cp(binding);
                let (val_form, _) = cp(val_rest);
                let var_name = sym_name(var_form);
                let val = eval_form(val_form, &mut child_env)?;
                if var_form.is_symbol() {
                    child_env.define_local_symbol(var_form, val);
                } else {
                    child_env.define_local(&var_name, val);
                }
            } else if binding.is_symbol() {
                child_env.define_local_symbol(binding, NIL);
            }
            c = rest;
        }
        return eval_progn(body, &mut child_env);
    }

    let mut evaluated = Vec::new();
    let mut c = bindings_form;
    while c.is_cons() {
        let (binding, rest) = cp(c);
        if binding.is_cons() {
            let (var_form, val_rest) = cp(binding);
            let (val_form, _) = cp(val_rest);
            evaluated.push((var_form, eval_form(val_form, env)?));
        } else if binding.is_symbol() {
            evaluated.push((binding, NIL));
        }
        c = rest;
    }

    let mut child_env = env.child();
    for (symbol, val) in evaluated {
        if symbol.is_symbol() {
            child_env.define_local_symbol(symbol, val);
        } else {
            child_env.define_local(&sym_name(symbol), val);
        }
    }
    eval_progn(body, &mut child_env)
}

// ── DEFUN ────────────────────────────────────────────────────────
fn eval_defun(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let (name_form, rest) = cp(cdr);
    let (params_form, body) = cp(rest);
    let name = sym_name(name_form);
    let params = extract_params(params_form);
    Rc::make_mut(&mut env.funs).insert(
        name.clone(),
        FunDef {
            params,
            params_form,
            body,
        },
    );
    Ok(name_form)
}

// ── FLET / LABELS local function binding ────────────────────────
// (flet ((name (lambda-list) body...) ...) body...)
// (labels ...) has the same shape but the local functions are mutually
// recursive. Both bind the named functions in a fresh child environment and
// evaluate the body there; because named functions in this evaluator resolve
// their names through the current environment's function table at call time,
// LABELS-style mutual recursion works naturally, and FLET-bound functions are
// visible only within the FLET/LABELS body.
fn eval_flet(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let (defs_form, body) = cp(cdr);
    let mut child_env = env.child();
    let mut c = defs_form;
    while c.is_cons() {
        let (def, rest) = cp(c);
        if def.is_cons() {
            let (name_form, def_rest) = cp(def);
            let (params_form, fbody) = cp(def_rest);
            let name = sym_name(name_form);
            let params = extract_params(params_form);
            Rc::make_mut(&mut child_env.funs).insert(
                name,
                FunDef {
                    params,
                    params_form,
                    body: fbody,
                },
            );
        }
        c = rest;
    }
    eval_progn(body, &mut child_env)
}

// ── Extract parameter names from a lambda list ──────────────────
fn extract_params(params_form: BlissVal) -> Vec<String> {
    let mut params = Vec::new();
    let mut c = params_form;
    while c.is_cons() {
        let (p, rest) = cp(c);
        if p.is_symbol() {
            let pname = sym_name(p);
            // Skip lambda-list keywords
            if !pname.starts_with('&') {
                params.push(pname);
            }
        }
        c = rest;
    }
    params
}

// ── Ordinary lambda-list binding ─────────────────────────────────
// Binds required, &optional (with defaults + supplied-p), &rest/&body,
// &key (with defaults, supplied-p, and ((:kw var) ...) form),
// &allow-other-keys (accepted, not enforced), and &aux. Defaults are
// evaluated left-to-right in `env` so they can see earlier parameters.

/// Bare (KEYWORD:-stripped) name of a symbol.
fn key_bare(sym: BlissVal) -> String {
    let n = sym_name(sym);
    n.strip_prefix("KEYWORD:").unwrap_or(&n).to_string()
}

/// Parse an &optional/&aux element: `var` | `(var [default [supplied-p]])`.
fn parse_var_spec(elem: BlissVal) -> (String, BlissVal, Option<String>) {
    if elem.is_symbol() {
        return (sym_name(elem), NIL, None);
    }
    if elem.is_cons() {
        let (var, r) = cp(elem);
        let (default, r2) = if r.is_cons() { cp(r) } else { (NIL, NIL) };
        let supp = if r2.is_cons() {
            Some(sym_name(cp(r2).0))
        } else {
            None
        };
        return (sym_name(var), default, supp);
    }
    (String::new(), NIL, None)
}

/// Parse a &key element: `var` | `(var [default [supp]])` | `((:kw var) [default [supp]])`.
/// Returns (keyword-bare-name, var-name, default-form, supplied-p-var).
fn parse_key_spec(elem: BlissVal) -> (String, String, BlissVal, Option<String>) {
    if elem.is_symbol() {
        let var = sym_name(elem);
        return (var.clone(), var, NIL, None);
    }
    if elem.is_cons() {
        let (head, r) = cp(elem);
        let (default, r2) = if r.is_cons() { cp(r) } else { (NIL, NIL) };
        let supp = if r2.is_cons() {
            Some(sym_name(cp(r2).0))
        } else {
            None
        };
        if head.is_symbol() {
            let var = sym_name(head);
            return (var.clone(), var, default, supp);
        }
        if head.is_cons() {
            let (kw_sym, r3) = cp(head);
            let var = if r3.is_cons() {
                sym_name(cp(r3).0)
            } else {
                String::new()
            };
            return (key_bare(kw_sym), var, default, supp);
        }
    }
    (String::new(), String::new(), NIL, None)
}

/// Look up a keyword's value in a `key value key value ...` argument tail.
fn find_key_arg(plist: &[BlissVal], kw_bare: &str) -> Option<BlissVal> {
    let mut i = 0;
    while i + 1 < plist.len() {
        if plist[i].is_symbol() && key_bare(plist[i]) == kw_bare {
            return Some(plist[i + 1]);
        }
        i += 2;
    }
    None
}

fn bind_lambda_list(
    params_form: BlissVal,
    args: &[BlissVal],
    env: &mut Env,
) -> Result<(), BlissError> {
    #[derive(PartialEq)]
    enum Mode {
        Req,
        Opt,
        Rest,
        Key,
        Aux,
    }
    let mut mode = Mode::Req;
    let mut arg_i = 0usize;
    let mut key_start: Option<usize> = None;
    let mut rest_bound = false;
    let mut saw_key = false;
    let mut allow_other_keys = false;
    let mut key_specs: Vec<(String, String, BlissVal, Option<String>)> = Vec::new();

    let mut c = params_form;
    while c.is_cons() {
        let (elem, rest) = cp(c);
        c = rest;
        if elem.is_symbol() {
            match sym_name(elem).as_str() {
                "&OPTIONAL" => {
                    mode = Mode::Opt;
                    continue;
                }
                "&REST" | "&BODY" => {
                    mode = Mode::Rest;
                    continue;
                }
                "&KEY" => {
                    mode = Mode::Key;
                    saw_key = true;
                    key_start.get_or_insert(arg_i);
                    continue;
                }
                "&AUX" => {
                    mode = Mode::Aux;
                    continue;
                }
                "&ALLOW-OTHER-KEYS" => {
                    allow_other_keys = true;
                    continue;
                }
                _ => {}
            }
        }
        match mode {
            Mode::Req => {
                let v = args.get(arg_i).copied().ok_or_else(|| {
                    BlissError::Internal(format!(
                        "too few arguments for lambda list: missing value for {}",
                        sym_name(elem)
                    ))
                })?;
                arg_i += 1;
                env.define_local_symbol(elem, v);
            }
            Mode::Opt => {
                let (var, default_form, supp) = parse_var_spec(elem);
                if arg_i < args.len() {
                    env.define_local(&var, args[arg_i]);
                    arg_i += 1;
                    if let Some(sp) = supp {
                        env.define_local(&sp, T);
                    }
                } else {
                    let dv = if default_form == NIL {
                        NIL
                    } else {
                        eval_form(default_form, env)?
                    };
                    env.define_local(&var, dv);
                    if let Some(sp) = supp {
                        env.define_local(&sp, NIL);
                    }
                }
            }
            Mode::Rest => {
                if rest_bound {
                    return Err(BlissError::Internal(
                        "malformed lambda list: multiple &rest/&body variables".into(),
                    ));
                }
                let remaining = args.get(arg_i..).unwrap_or(&[]);
                env.define_local(&sym_name(elem), vec_to_list(remaining));
                key_start.get_or_insert(arg_i);
                rest_bound = true;
            }
            Mode::Key => {
                let (kw_bare, var, default_form, supp) = parse_key_spec(elem);
                key_specs.push((kw_bare, var, default_form, supp));
            }
            Mode::Aux => {
                let (var, default_form, _) = parse_var_spec(elem);
                let dv = if default_form == NIL {
                    NIL
                } else {
                    eval_form(default_form, env)?
                };
                env.define_local(&var, dv);
            }
        }
    }

    if saw_key {
        let start = key_start.unwrap_or(arg_i);
        let tail = args.get(start..).unwrap_or(&[]);
        if tail.len() % 2 != 0 {
            return Err(BlissError::Internal(
                "keyword arguments must appear in key/value pairs".into(),
            ));
        }

        let mut call_allows_other_keys = false;
        for pair in tail.chunks(2) {
            let key = pair[0];
            if !key.is_symbol() {
                return Err(BlissError::TypeError {
                    datum: key,
                    expected: "keyword".into(),
                });
            }
            let bare = key_bare(key);
            if bare == "ALLOW-OTHER-KEYS" && !pair[1].is_nil() {
                call_allows_other_keys = true;
            }
        }

        for (kw_bare, var, default_form, supp) in &key_specs {
            if let Some(v) = find_key_arg(tail, kw_bare) {
                env.define_local(var, v);
                if let Some(sp) = supp {
                    env.define_local(sp, T);
                }
            } else {
                let dv = if *default_form == NIL {
                    NIL
                } else {
                    eval_form(*default_form, env)?
                };
                env.define_local(var, dv);
                if let Some(sp) = supp {
                    env.define_local(sp, NIL);
                }
            }
        }

        if !(allow_other_keys || call_allows_other_keys) {
            for pair in tail.chunks(2) {
                let bare = key_bare(pair[0]);
                if bare == "ALLOW-OTHER-KEYS" {
                    continue;
                }
                if !key_specs.iter().any(|(kw, _, _, _)| kw == &bare) {
                    return Err(BlissError::Internal(format!(
                        "unexpected keyword argument: {}",
                        bare
                    )));
                }
            }
        }
    } else if !rest_bound && arg_i < args.len() {
        return Err(BlissError::Internal(format!(
            "too many arguments for lambda list: expected {}, got {}",
            arg_i,
            args.len()
        )));
    }

    Ok(())
}

fn bind_pattern_value(pattern: BlissVal, value: BlissVal, env: &mut Env) -> Result<(), BlissError> {
    if pattern.is_nil() {
        if value.is_nil() {
            return Ok(());
        }
        return Err(BlissError::Internal(format!(
            "destructuring mismatch: expected NIL, got {}",
            format_val(value)
        )));
    }

    if pattern.is_symbol() {
        env.define_local_symbol(pattern, value);
        return Ok(());
    }

    if !pattern.is_cons() {
        return Err(BlissError::Internal(format!(
            "invalid destructuring pattern: {}",
            format_val(pattern)
        )));
    }

    if !value.is_cons() {
        return Err(BlissError::Internal(format!(
            "destructuring mismatch: expected list for pattern {}, got {}",
            format_val(pattern),
            format_val(value)
        )));
    }

    let (pcar, pcdr) = cp(pattern);
    let (vcar, vcdr) = cp(value);
    bind_pattern_value(pcar, vcar, env)?;
    bind_pattern_value(pcdr, vcdr, env)
}

fn bind_macro_lambda_list(
    params_form: BlissVal,
    args: &[BlissVal],
    env: &mut Env,
    macroexpand_env: Option<&MacroexpandEnv>,
) -> Result<(), BlissError> {
    #[derive(PartialEq)]
    enum Mode {
        Req,
        Opt,
        Rest,
        Key,
        Aux,
    }

    let mut mode = Mode::Req;
    let mut arg_i = 0usize;
    let mut key_start: Option<usize> = None;
    let mut rest_bound = false;
    let mut saw_key = false;
    let mut allow_other_keys = false;
    let mut key_specs: Vec<(String, BlissVal, BlissVal, Option<String>)> = Vec::new();
    let mut whole_var: Option<BlissVal> = None;
    let whole_form = vec_to_list(args);

    let mut c = params_form;
    while c.is_cons() {
        let (elem, rest) = cp(c);
        c = rest;

        if elem.is_symbol() {
            match sym_name(elem).as_str() {
                "&WHOLE" => {
                    let (var, rest_after_var) = cp(c);
                    whole_var = Some(var);
                    c = rest_after_var;
                    continue;
                }
                "&ENVIRONMENT" => {
                    let (var, rest_after_var) = cp(c);
                    let env_value = macroexpand_env
                        .cloned()
                        .map(store_macroexpand_environment)
                        .unwrap_or(NIL);
                    bind_pattern_value(var, env_value, env)?;
                    c = rest_after_var;
                    continue;
                }
                "&OPTIONAL" => {
                    mode = Mode::Opt;
                    continue;
                }
                "&REST" | "&BODY" => {
                    mode = Mode::Rest;
                    continue;
                }
                "&KEY" => {
                    mode = Mode::Key;
                    saw_key = true;
                    key_start.get_or_insert(arg_i);
                    continue;
                }
                "&AUX" => {
                    mode = Mode::Aux;
                    continue;
                }
                "&ALLOW-OTHER-KEYS" => {
                    allow_other_keys = true;
                    continue;
                }
                _ => {}
            }
        }

        match mode {
            Mode::Req => {
                let v = args.get(arg_i).copied().ok_or_else(|| {
                    BlissError::Internal(format!(
                        "too few arguments for macro lambda list: missing value for {}",
                        format_val(elem)
                    ))
                })?;
                arg_i += 1;
                bind_pattern_value(elem, v, env)?;
            }
            Mode::Opt => {
                let (pattern, default_form, supp) = if elem.is_symbol() {
                    (elem, NIL, None)
                } else if elem.is_cons() {
                    let (pat, r) = cp(elem);
                    let (default, r2) = if r.is_cons() { cp(r) } else { (NIL, NIL) };
                    let supp = if r2.is_cons() {
                        Some(sym_name(cp(r2).0))
                    } else {
                        None
                    };
                    (pat, default, supp)
                } else {
                    (elem, NIL, None)
                };

                if arg_i < args.len() {
                    bind_pattern_value(pattern, args[arg_i], env)?;
                    arg_i += 1;
                    if let Some(sp) = supp {
                        env.define_local(&sp, T);
                    }
                } else {
                    let dv = if default_form == NIL {
                        NIL
                    } else {
                        eval_form(default_form, env)?
                    };
                    bind_pattern_value(pattern, dv, env)?;
                    if let Some(sp) = supp {
                        env.define_local(&sp, NIL);
                    }
                }
            }
            Mode::Rest => {
                if rest_bound {
                    return Err(BlissError::Internal(
                        "malformed macro lambda list: multiple &rest/&body variables".into(),
                    ));
                }
                let remaining = vec_to_list(args.get(arg_i..).unwrap_or(&[]));
                bind_pattern_value(elem, remaining, env)?;
                key_start.get_or_insert(arg_i);
                rest_bound = true;
            }
            Mode::Key => {
                let (kw_bare, pattern, default_form, supp) = if elem.is_symbol() {
                    let bare = sym_name(elem);
                    (bare, elem, NIL, None)
                } else if elem.is_cons() {
                    let (head, r) = cp(elem);
                    let (default, r2) = if r.is_cons() { cp(r) } else { (NIL, NIL) };
                    let supp = if r2.is_cons() {
                        Some(sym_name(cp(r2).0))
                    } else {
                        None
                    };
                    if head.is_symbol() {
                        (sym_name(head), head, default, supp)
                    } else if head.is_cons() {
                        let (kw_sym, r3) = cp(head);
                        let pattern = if r3.is_cons() { cp(r3).0 } else { NIL };
                        (key_bare(kw_sym), pattern, default, supp)
                    } else {
                        (String::new(), head, default, supp)
                    }
                } else {
                    (String::new(), elem, NIL, None)
                };
                key_specs.push((kw_bare, pattern, default_form, supp));
            }
            Mode::Aux => {
                let (pattern, default_form) = if elem.is_symbol() {
                    (elem, NIL)
                } else if elem.is_cons() {
                    let (pat, r) = cp(elem);
                    let (default, _) = if r.is_cons() { cp(r) } else { (NIL, NIL) };
                    (pat, default)
                } else {
                    (elem, NIL)
                };
                let dv = if default_form == NIL {
                    NIL
                } else {
                    eval_form(default_form, env)?
                };
                bind_pattern_value(pattern, dv, env)?;
            }
        }
    }

    if let Some(var) = whole_var {
        bind_pattern_value(var, whole_form, env)?;
    }

    if saw_key {
        let start = key_start.unwrap_or(arg_i);
        let tail = args.get(start..).unwrap_or(&[]);
        if tail.len() % 2 != 0 {
            return Err(BlissError::Internal(
                "macro keyword arguments must appear in key/value pairs".into(),
            ));
        }

        let mut call_allows_other_keys = false;
        for pair in tail.chunks(2) {
            let key = pair[0];
            if !key.is_symbol() {
                return Err(BlissError::TypeError {
                    datum: key,
                    expected: "keyword".into(),
                });
            }
            let bare = key_bare(key);
            if bare == "ALLOW-OTHER-KEYS" && !pair[1].is_nil() {
                call_allows_other_keys = true;
            }
        }

        for (kw_bare, pattern, default_form, supp) in &key_specs {
            if let Some(v) = find_key_arg(tail, kw_bare) {
                bind_pattern_value(*pattern, v, env)?;
                if let Some(sp) = supp {
                    env.define_local(sp, T);
                }
            } else {
                let dv = if *default_form == NIL {
                    NIL
                } else {
                    eval_form(*default_form, env)?
                };
                bind_pattern_value(*pattern, dv, env)?;
                if let Some(sp) = supp {
                    env.define_local(sp, NIL);
                }
            }
        }

        if !(allow_other_keys || call_allows_other_keys) {
            for pair in tail.chunks(2) {
                let bare = key_bare(pair[0]);
                if bare == "ALLOW-OTHER-KEYS" {
                    continue;
                }
                if !key_specs.iter().any(|(kw, _, _, _)| kw == &bare) {
                    return Err(BlissError::Internal(format!(
                        "unexpected macro keyword argument: {}",
                        bare
                    )));
                }
            }
        }
    } else if !rest_bound && arg_i < args.len() {
        return Err(BlissError::Internal(format!(
            "too many arguments for macro lambda list: expected {}, got {}",
            arg_i,
            args.len()
        )));
    }

    Ok(())
}

// ── DEFMACRO ─────────────────────────────────────────────────────
fn eval_defmacro(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let (name_form, rest) = cp(cdr);
    let (params_form, body) = cp(rest);
    let name = sym_name(name_form);

    Rc::make_mut(&mut env.macros).insert(
        name.clone(),
        MacroDef {
            params_form,
            body,
            captured_frame: Rc::clone(&env.frame),
        },
    );
    Ok(name_form)
}

fn eval_define_symbol_macro(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let (symbol, rest) = cp(cdr);
    let (expansion, _) = cp(rest);
    if !symbol.is_symbol() {
        return Err(BlissError::Internal(
            "DEFINE-SYMBOL-MACRO: name must be a symbol".into(),
        ));
    }
    env.define_symbol_macro(symbol, expansion);
    Ok(symbol)
}

fn eval_define_compiler_macro(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let (name_form, rest) = cp(cdr);
    let (params_form, body) = cp(rest);
    if !name_form.is_symbol() {
        return Err(BlissError::Internal(
            "DEFINE-COMPILER-MACRO: name must be a symbol".into(),
        ));
    }

    let captured_frame = freeze_env_frame(&env.frame);
    let funs = (*env.funs).clone();
    let classes = (*env.classes).clone();
    let methods = (*env.methods).clone();
    let packages = (*env.packages).clone();
    let current_package = env.current_package.clone();
    let sandbox = env.sandbox;
    let symbol_macros = (*env.symbol_macros).clone();
    let eval_context = env.eval_context;

    compiler_macroexpand::define_compiler_macro(
        name_form,
        Arc::new(move |form, _macro_env| {
            let (_, args) = cp(form);
            let mut macro_env = Env::new(sandbox);
            macro_env.frame = thaw_env_frame(&captured_frame);
            macro_env.funs = Rc::new(funs.clone());
            macro_env.macros = Rc::new(HashMap::new());
            macro_env.symbol_macros = Rc::new(symbol_macros.clone());
            macro_env.classes = Rc::new(classes.clone());
            macro_env.methods = Rc::new(methods.clone());
            macro_env.packages = Rc::new(packages.clone());
            macro_env.current_package = current_package.clone();
            macro_env.eval_context = eval_context;
            bind_macro_lambda_list(
                params_form,
                &list_to_vec(args),
                &mut macro_env,
                Some(_macro_env),
            )?;
            eval_progn(body, &mut macro_env)
        }),
    );

    Ok(name_form)
}

fn expand_macro(mdef: &MacroDef, args: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let mut child_env = env.child_with_parent(Rc::clone(&mdef.captured_frame));
    let arg_list = list_to_vec(args);
    let macroexpand_env = macroexpand_environment_from_cli(env);
    bind_macro_lambda_list(
        mdef.params_form,
        &arg_list,
        &mut child_env,
        Some(&macroexpand_env),
    )?;
    eval_progn(mdef.body, &mut child_env)
}

fn macroexpand_environment_from_cli(env: &Env) -> MacroexpandEnv {
    fn collect_frames(frame: &Rc<RefCell<EnvFrame>>, frames: &mut Vec<Rc<RefCell<EnvFrame>>>) {
        let parent = frame.borrow().parent.clone();
        if let Some(parent) = parent {
            collect_frames(&parent, frames);
        }
        frames.push(Rc::clone(frame));
    }

    let mut macro_env = MacroexpandEnv::null();

    let mut global_symbol_macros = Vec::new();
    for (&symbol_index, &expansion) in env.symbol_macros.iter() {
        global_symbol_macros.push((
            BlissVal::from_symbol_index(symbol_index),
            VariableInfo::SymbolMacro(expansion),
        ));
    }
    if !global_symbol_macros.is_empty() {
        macro_env = macro_env.augment_environment(global_symbol_macros, Vec::new(), Vec::new());
    }

    let mut frames = Vec::new();
    collect_frames(&env.frame, &mut frames);
    for frame in frames {
        let borrowed = frame.borrow();
        let mut variables = Vec::new();
        for &symbol_index in borrowed.symbol_vars.keys() {
            variables.push((
                BlissVal::from_symbol_index(symbol_index),
                VariableInfo::Lexical,
            ));
        }
        if !variables.is_empty() {
            macro_env = macro_env.augment_environment(variables, Vec::new(), Vec::new());
        }
    }

    for (name, macro_def) in env.macros.iter() {
        let handle = next_macro_function_handle();
        let params_form = macro_def.params_form;
        let body = macro_def.body;
        let captured_frame = freeze_env_frame(&macro_def.captured_frame);
        compiler_macroexpand::register_macro_function(
            handle,
            Arc::new(move |form, call_macro_env| {
                let (_, args) = cp(form);
                let mut macro_env = Env::new(false);
                macro_env.frame = thaw_env_frame(&captured_frame);
                bind_macro_lambda_list(
                    params_form,
                    &list_to_vec(args),
                    &mut macro_env,
                    Some(call_macro_env),
                )?;
                eval_progn(body, &mut macro_env)
            }),
        );
        if let Some(symbol) = resolve_sym(name) {
            macro_env = macro_env.augment_function(symbol, FunctionInfo::Macro(handle));
        }
    }

    macro_env
}

fn eval_macroexpand(
    cdr: BlissVal,
    env: &mut Env,
    single_step: bool,
) -> Result<BlissVal, BlissError> {
    let (form_expr, rest) = cp(cdr);
    let form = eval_form(form_expr, env)?;
    let macro_env = if rest.is_cons() {
        let (env_expr, _) = cp(rest);
        let env_value = eval_form(env_expr, env)?;
        load_macroexpand_environment(env_value).ok_or_else(|| {
            BlissError::Internal("MACROEXPAND: invalid lexical environment".into())
        })?
    } else {
        macroexpand_environment_from_cli(env)
    };
    let (expanded, expanded_p) = if single_step {
        compiler_macroexpand::macroexpand_1(form, &macro_env)?
    } else {
        compiler_macroexpand::macroexpand(form, &macro_env)?
    };
    env.set_mv(vec![expanded, if expanded_p { T } else { NIL }]);
    Ok(expanded)
}

fn eval_macrolet(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let (defs_form, body) = cp(cdr);
    let mut child_env = env.child();
    for def in list_to_vec(defs_form) {
        if !def.is_cons() {
            continue;
        }
        let (name_form, rest) = cp(def);
        let (params_form, macro_body) = cp(rest);
        let name = sym_name(name_form);
        Rc::make_mut(&mut child_env.macros).insert(
            name,
            MacroDef {
                params_form,
                body: macro_body,
                captured_frame: Rc::clone(&env.frame),
            },
        );
    }
    eval_progn(body, &mut child_env)
}

fn eval_symbol_macrolet(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let (bindings_form, body) = cp(cdr);
    let mut child_env = env.child();
    for binding in list_to_vec(bindings_form) {
        if !binding.is_cons() {
            continue;
        }
        let (symbol, expansion_rest) = cp(binding);
        if !symbol.is_symbol() {
            continue;
        }
        let (expansion, _) = cp(expansion_rest);
        child_env.define_symbol_macro(symbol, expansion);
    }
    eval_progn(body, &mut child_env)
}

// ── DEFCLASS ─────────────────────────────────────────────────────
fn eval_defclass(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let (name_form, rest) = cp(cdr);
    let (supers_form, rest2) = cp(rest);
    let (slots_form, _) = cp(rest2);

    let name = sym_name(name_form);

    // Parse superclasses
    let mut supers = Vec::new();
    let super_list = list_to_vec(supers_form);
    for s in &super_list {
        supers.push(sym_name(*s));
    }

    // Parse slots
    let mut slots = Vec::new();
    let slot_list = list_to_vec(slots_form);
    for slot_form in &slot_list {
        if slot_form.is_cons() {
            let (slot_name_form, slot_opts) = cp(*slot_form);
            let slot_name = sym_name(slot_name_form);
            let mut initarg = None;
            let mut accessor = None;
            let mut readers = Vec::new();
            let mut writers = Vec::new();
            let mut initform = None;
            let mut allocation = SlotAllocation::Instance;

            // Parse slot options
            let opts = list_to_vec(slot_opts);
            let mut i = 0;
            while i < opts.len() {
                let opt_name = sym_name(opts[i]);
                let opt_bare = opt_name
                    .trim_start_matches("KEYWORD:")
                    .trim_start_matches(':');
                if opt_bare == "INITARG" {
                    if i + 1 < opts.len() {
                        let ia = sym_name(opts[i + 1]);
                        initarg = Some(
                            ia.trim_start_matches("KEYWORD:")
                                .trim_start_matches(':')
                                .to_string(),
                        );
                        i += 2;
                    } else {
                        i += 1;
                    }
                } else if opt_bare == "ACCESSOR" {
                    if i + 1 < opts.len() {
                        let accessor_name = sym_name(opts[i + 1]);
                        accessor = Some(accessor_name.clone());
                        readers.push(accessor_name);
                        i += 2;
                    } else {
                        i += 1;
                    }
                } else if opt_bare == "READER" {
                    if i + 1 < opts.len() {
                        readers.push(sym_name(opts[i + 1]));
                        i += 2;
                    } else {
                        i += 1;
                    }
                } else if opt_bare == "WRITER" {
                    if i + 1 < opts.len() {
                        if opts[i + 1].is_symbol() {
                            writers.push(sym_name(opts[i + 1]));
                        }
                        i += 2;
                    } else {
                        i += 1;
                    }
                } else if opt_bare == "INITFORM" {
                    if i + 1 < opts.len() {
                        initform = Some(opts[i + 1]);
                        i += 2;
                    } else {
                        i += 1;
                    }
                } else if opt_bare == "ALLOCATION" {
                    if i + 1 < opts.len() {
                        let allocation_name = symbol_bare_name(&sym_name(opts[i + 1]));
                        if allocation_name == "CLASS" {
                            allocation = SlotAllocation::Class;
                        }
                        i += 2;
                    } else {
                        i += 1;
                    }
                } else if opt_bare == "TYPE" || opt_bare == "DOCUMENTATION" {
                    i += 2;
                } else {
                    i += 1;
                }
            }

            slots.push(SlotDef {
                name: slot_name,
                initarg,
                accessor,
                readers,
                writers,
                initform,
                allocation,
            });
        } else if slot_form.is_symbol() {
            slots.push(SlotDef {
                name: sym_name(*slot_form),
                initarg: None,
                accessor: None,
                readers: Vec::new(),
                writers: Vec::new(),
                initform: None,
                allocation: SlotAllocation::Instance,
            });
        }
    }

    let mut class_slot_values = HashMap::new();
    for slot in &slots {
        if slot.allocation == SlotAllocation::Class {
            class_slot_values.insert(slot.name.clone(), None);
        }
    }

    Rc::make_mut(&mut env.classes).insert(
        name.clone(),
        ClassDef {
            name: name.clone(),
            supers,
            slots,
            class_slot_values: Arc::new(Mutex::new(class_slot_values)),
        },
    );
    let direct_supers: Result<Vec<BlissVal>, BlissError> = super_list
        .iter()
        .map(|super_name| resolve_class_metaobject(env, *super_name))
        .collect();
    let slot_names: Vec<BlissVal> = env.classes[&name]
        .slots
        .iter()
        .map(|slot| resolve_sym(&slot.name).unwrap_or(NIL))
        .collect();
    bliss_stdlib::define_class(name_form, name_form, &direct_supers?, &slot_names)?;
    Ok(name_form)
}

// ── DEFGENERIC ───────────────────────────────────────────────────
fn eval_defgeneric(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let (name_form, options) = cp(cdr);
    let name = sym_name(name_form);
    let mut combination = bliss_stdlib::MethodCombinationType::Standard;
    let mut opts = options;
    while opts.is_cons() {
        let (option, rest) = cp(opts);
        if option.is_cons() {
            let (option_name, option_rest) = cp(option);
            if symbol_bare_name(&sym_name(option_name)) == "METHOD-COMBINATION" {
                let method_combination = cp(option_rest).0;
                combination = method_combination_from_name(&sym_name(method_combination))
                    .unwrap_or(bliss_stdlib::MethodCombinationType::Standard);
            }
        }
        opts = rest;
    }
    let generic_function = bliss_stdlib::make_generic_function(name_form, NIL)?;
    Rc::make_mut(&mut env.generics).insert(
        name.clone(),
        GenericDef {
            generic_function,
            combination,
        },
    );
    Rc::make_mut(&mut env.methods).entry(name).or_default();
    Ok(name_form)
}

// ── DEFMETHOD ────────────────────────────────────────────────────
fn eval_defmethod(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let (name_form, rest) = cp(cdr);
    let name = sym_name(name_form);
    let combination = env
        .generics
        .get(&name)
        .map(|generic| generic.combination)
        .unwrap_or(bliss_stdlib::MethodCombinationType::Standard);
    let mut cursor = rest;
    let mut qualifier = bliss_stdlib::MethodQualifier::Primary;
    while cursor.is_cons() {
        let (head, tail) = cp(cursor);
        if head.is_cons() {
            cursor = arena_cons(head, tail);
            break;
        }
        if !head.is_symbol() {
            break;
        }
        match symbol_bare_name(&sym_name(head)).as_str() {
            "AROUND" => qualifier = bliss_stdlib::MethodQualifier::Around,
            "BEFORE" => qualifier = bliss_stdlib::MethodQualifier::Before,
            "AFTER" => qualifier = bliss_stdlib::MethodQualifier::After,
            qualifier_name
                if combination != bliss_stdlib::MethodCombinationType::Standard
                    && method_combination_from_name(qualifier_name) == Some(combination) => {}
            _ => break,
        }
        cursor = tail;
    }
    let (spec_params_form, body) = cp(cursor);
    let method_id = next_stdlib_class_id();

    // Parse specialized params: ((var class) ...)
    let params_list = list_to_vec(spec_params_form);
    let mut params = Vec::new();
    let mut specializers = Vec::new();

    for p in &params_list {
        if p.is_cons() {
            let (var_form, rest_p) = cp(*p);
            let (class_form, _) = cp(rest_p);
            params.push(sym_name(var_form));
            if class_form.is_cons() {
                let (head, value_rest) = cp(class_form);
                if head.is_symbol() && symbol_bare_name(&sym_name(head)) == "EQL" {
                    specializers.push(MethodSpecializer::Eql(eval_form(cp(value_rest).0, env)?));
                } else {
                    specializers.push(MethodSpecializer::Class(sym_name(class_form)));
                }
            } else {
                let specializer_name = sym_name(class_form);
                if symbol_bare_name(&specializer_name) == "T" {
                    specializers.push(MethodSpecializer::Any);
                } else {
                    specializers.push(MethodSpecializer::Class(specializer_name));
                }
            }
        } else if p.is_symbol() {
            params.push(sym_name(*p));
            specializers.push(MethodSpecializer::Any);
        }
    }

    let generic_function = if let Some(generic) = env.generics.get(&name) {
        generic.generic_function
    } else {
        let gf = bliss_stdlib::make_generic_function(name_form, NIL)?;
        Rc::make_mut(&mut env.generics).insert(
            name.clone(),
            GenericDef {
                generic_function: gf,
                combination: bliss_stdlib::MethodCombinationType::Standard,
            },
        );
        gf
    };
    bliss_stdlib::clos::add_method(generic_function, method_id)?;
    bliss_stdlib::set_method_specializers(method_id, vec![], qualifier);

    Rc::make_mut(&mut env.methods)
        .entry(name.clone())
        .or_default()
        .push(MethodDef {
            method_id,
            specializers,
            params,
            body,
        });
    Ok(name_form)
}

// ── MAKE-INSTANCE ────────────────────────────────────────────────
fn eval_make_instance(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let (class_form, init_args) = cp(cdr);
    let class_input = eval_form(class_form, env)?;
    let class = resolve_class_metaobject(env, class_input)?;
    let class_name = class_name_for_instance_class(class);
    let initargs = evaluated_initargs(&class_name, init_args, env)?;
    let explicit_slots: Vec<String> = initargs
        .chunks_exact(2)
        .map(|pair| symbol_bare_name(&sym_name(pair[0])))
        .collect();
    let (instance_initargs, class_initargs) = split_initargs_for_class(env, &class_name, &initargs);
    let instance = bliss_stdlib::make_instance(class, &instance_initargs)?;
    for (slot_name, value) in class_initargs {
        write_class_slot_value(env, &class_name, &slot_name, Some(value));
    }
    apply_class_initforms(instance, &class_name, env, None, &explicit_slots)?;
    Ok(instance)
}

// ── Apply function (lambda or named) ─────────────────────────────
fn apply_function(
    fn_val: BlissVal,
    args: &[BlissVal],
    env: &mut Env,
) -> Result<BlissVal, BlissError> {
    // Function could be a lambda form, a symbol naming a function, or a closure
    if fn_val.is_symbol() {
        let name = sym_name(fn_val);
        if let Some(fdef) = env.funs.get(&name).cloned() {
            return eval_lambda_call(
                env,
                fdef.params_form,
                fdef.body,
                args,
                Rc::clone(&env.frame),
            );
        }
        if env.generics.contains_key(&name) || env.methods.contains_key(&name) {
            return invoke_generic_function(&name, args, env);
        }
        // Builtin: synthesize `(name 'arg1 'arg2 ...)` and evaluate it so the
        // full operator-position builtin set (not just apply_builtin's subset)
        // is reachable through funcall/apply/mapcar.
        let quote_sym = resolve_sym("QUOTE").unwrap_or(NIL);
        let mut items = Vec::with_capacity(args.len() + 1);
        items.push(fn_val);
        for a in args {
            items.push(arena_cons(quote_sym, arena_cons(*a, NIL)));
        }
        let form = vec_to_list(&items);
        return eval_form(form, env);
    }
    if fn_val.is_cons() {
        let (lh, lr) = cp(fn_val);
        // Check for closure: (BLISS::CLOSURE . id)
        if lh.is_symbol() && sym_name(lh) == "BLISS::CLOSURE" && lr.is_fixnum() {
            let id = lr.as_fixnum() as u64;
            let closure = { env.closures.borrow().get(&id).cloned() };
            if let Some(closure) = closure {
                return eval_lambda_call(
                    env,
                    closure.params_form,
                    closure.body,
                    args,
                    Rc::clone(&closure.captured_frame),
                );
            }
        }
        if lh.is_symbol() && sym_name(lh) == "LAMBDA" {
            let (params_form, body) = cp(lr);
            return eval_lambda_call(env, params_form, body, args, Rc::clone(&env.frame));
        }
    }
    Err(BlissError::Internal(format!("Cannot apply: {:?}", fn_val)))
}

#[expect(
    dead_code,
    reason = "legacy builtin dispatch is retained during evaluator consolidation"
)]
fn apply_builtin(name: &str, args: &[BlissVal], _env: &mut Env) -> Result<BlissVal, BlissError> {
    match name {
        "+" => {
            let mut sum: f64 = 0.0;
            let mut is_f = false;
            for a in args {
                let v = num_val(*a)?;
                sum += v;
                if a.is_single_float() {
                    is_f = true;
                }
            }
            Ok(if is_f {
                BlissVal::from_single_float(sum as f32)
            } else {
                BlissVal::from_fixnum(sum as i64)
            })
        }
        "-" => {
            if args.is_empty() {
                return Ok(BlissVal::from_fixnum(0));
            }
            if args.len() == 1 {
                let v = num_val(args[0])?;
                return Ok(BlissVal::from_fixnum((-v) as i64));
            }
            let mut acc = num_val(args[0])?;
            let mut is_f = args[0].is_single_float();
            for a in &args[1..] {
                acc -= num_val(*a)?;
                if a.is_single_float() {
                    is_f = true;
                }
            }
            Ok(if is_f {
                BlissVal::from_single_float(acc as f32)
            } else {
                BlissVal::from_fixnum(acc as i64)
            })
        }
        "*" => {
            let mut prod: f64 = 1.0;
            let mut is_f = false;
            for a in args {
                let v = num_val(*a)?;
                prod *= v;
                if a.is_single_float() {
                    is_f = true;
                }
            }
            Ok(if is_f {
                BlissVal::from_single_float(prod as f32)
            } else {
                BlissVal::from_fixnum(prod as i64)
            })
        }
        "CONS" => {
            if args.len() >= 2 {
                Ok(arena_cons(args[0], args[1]))
            } else {
                Err(BlissError::Internal("CONS requires 2 arguments".into()))
            }
        }
        "CAR" | "FIRST" => {
            if args.is_empty() {
                return Ok(NIL);
            }
            let v = args[0];
            if v.is_nil() {
                return Ok(NIL);
            }
            if v.is_cons() {
                let (a, _) = cp(v);
                return Ok(a);
            }
            Err(BlissError::TypeError {
                datum: v,
                expected: "list".into(),
            })
        }
        "CDR" | "REST" => {
            if args.is_empty() {
                return Ok(NIL);
            }
            let v = args[0];
            if v.is_nil() {
                return Ok(NIL);
            }
            if v.is_cons() {
                let (_, d) = cp(v);
                return Ok(d);
            }
            Err(BlissError::TypeError {
                datum: v,
                expected: "list".into(),
            })
        }
        _ => Err(BlissError::UndefinedFunction(
            resolve_sym(name).unwrap_or(NIL),
        )),
    }
}

// ── FLOOR ────────────────────────────────────────────────────────
fn eval_floor(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let (af, r) = cp(cdr);
    let a = eval_form(af, env)?;
    let av = num_val(a)?;
    if r.is_cons() {
        let (bf, _) = cp(r);
        let b = eval_form(bf, env)?;
        let bv = num_val(b)?;
        if bv == 0.0 {
            return Err(BlissError::ArithmeticError("division by zero".into()));
        }
        let q = (av / bv).floor() as i64;
        let rem = av - (q as f64) * bv;
        env.set_mv(vec![
            BlissVal::from_fixnum(q),
            BlissVal::from_fixnum(rem as i64),
        ]);
        return Ok(BlissVal::from_fixnum(q));
    }
    let q = av.floor() as i64;
    let rem = av - q as f64;
    env.set_mv(vec![
        BlissVal::from_fixnum(q),
        BlissVal::from_single_float(rem as f32),
    ]);
    Ok(BlissVal::from_fixnum(q))
}

// ── MULTIPLE-VALUE-BIND ─────────────────────────────────────────
fn eval_multiple_value_bind(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let (vars_form, rest) = cp(cdr);
    let (values_form, body) = cp(rest);

    // Evaluate the values form
    env.clear_mv();
    let primary = eval_form(values_form, env)?;
    let mv = env.mv.clone();

    // Bind variables
    let var_names: Vec<String> = list_to_vec(vars_form)
        .iter()
        .map(|v| sym_name(*v))
        .collect();
    let mut child_env = env.child();

    for (i, var_name) in var_names.iter().enumerate() {
        let val = if i == 0 {
            primary
        } else if i < mv.len() {
            mv[i]
        } else {
            NIL
        };
        child_env.define_local(var_name, val);
    }

    eval_progn(body, &mut child_env)
}

// ── HANDLER-CASE ────────────────────────────────────────────────
fn eval_handler_case(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let (protected_form, clauses) = cp(cdr);
    let base_len = env.handlers.len();
    let mut installed = Vec::new();
    let mut c = clauses;
    while c.is_cons() {
        let (clause, rest) = cp(c);
        let (type_form, clause_rest) = cp(clause);
        let (bind_list, handler_body) = cp(clause_rest);
        let token = next_control_token("handler-case");
        let var_name = if bind_list.is_cons() {
            Some(sym_name(cp(bind_list).0))
        } else {
            None
        };
        let entry = HandlerEntry {
            type_name: sym_name(type_form),
            handler: HandlerImpl::HandlerCase {
                token: token.clone(),
                var_name: var_name.clone(),
                body: handler_body,
                captured_frame: freeze_env_frame(&env.frame),
            },
        };
        env.handlers.push(entry.clone());
        installed.push(entry);
        c = rest;
    }

    let result = eval_form(protected_form, env);
    env.handlers.truncate(base_len);

    match result {
        Ok(val) => Ok(val),
        Err(error) => {
            let Some(token) = handler_case_token(&error) else {
                return Err(error);
            };
            let condition = take_control_value(&token);
            for handler in installed {
                if let HandlerImpl::HandlerCase {
                    token: entry_token,
                    var_name,
                    body,
                    captured_frame,
                } = handler.handler
                {
                    if entry_token == token {
                        let mut handler_env = env.child_with_parent(thaw_env_frame(&captured_frame));
                        if let Some(name) = var_name {
                            handler_env.define_local(&name, condition);
                        }
                        return eval_progn(body, &mut handler_env);
                    }
                }
            }
            Err(error)
        }
    }
}

// ── HANDLER-BIND ────────────────────────────────────────────────
fn eval_handler_bind(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let (bindings_form, body) = cp(cdr);

    // Parse handler bindings and install them
    let mut c = bindings_form;
    while c.is_cons() {
        let (binding, rest) = cp(c);
        let (type_form, handler_rest) = cp(binding);
        let (handler_form, _) = cp(handler_rest);
        let type_name = sym_name(type_form);
        env.handlers.push(HandlerEntry {
            type_name,
            handler: HandlerImpl::Function(handler_form),
        });
        c = rest;
    }

    let result = eval_progn(body, env);

    // Clean up handlers (pop what we added)
    let binding_count = list_to_vec(bindings_form).len();
    for _ in 0..binding_count {
        env.handlers.pop();
    }

    result
}

fn parse_restart_options(
    option_forms: BlissVal,
    captured_frame: &Arc<FrozenEnvFrame>,
) -> (Option<RestartFunction>, Option<RestartFunction>) {
    let options = list_to_vec(option_forms);
    let mut interactive_function = None;
    let mut test_function = None;
    let mut index = 0;
    while index + 1 < options.len() {
        let key = options[index];
        let value = options[index + 1];
        if key.is_symbol() {
            match symbol_bare_name(&sym_name(key)).as_str() {
                "INTERACTIVE-FUNCTION" => {
                    interactive_function = Some(RestartFunction::FunctionForm {
                        function_form: value,
                        captured_frame: captured_frame.clone(),
                    });
                }
                "TEST-FUNCTION" => {
                    test_function = Some(RestartFunction::FunctionForm {
                        function_form: value,
                        captured_frame: captured_frame.clone(),
                    });
                }
                _ => {}
            }
        }
        index += 2;
    }
    (interactive_function, test_function)
}

// ── RESTART-BIND ────────────────────────────────────────────────
fn eval_restart_bind(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let (bindings_form, body) = cp(cdr);
    let base_len = env.restarts.len();
    let captured_frame = freeze_env_frame(&env.frame);
    let mut c = bindings_form;
    while c.is_cons() {
        let (binding, rest) = cp(c);
        let (name_form, binding_rest) = cp(binding);
        let (function_form, option_forms) = cp(binding_rest);
        let (interactive_function, test_function) =
            parse_restart_options(option_forms, &captured_frame);
        env.restarts.push(RestartEntry {
            name: sym_name(name_form).to_uppercase(),
            function: RestartFunction::FunctionForm {
                function_form,
                captured_frame: captured_frame.clone(),
            },
            interactive_function,
            test_function,
            unwind_on_invoke: false,
        });
        c = rest;
    }

    let result = eval_progn(body, env);
    env.restarts.truncate(base_len);
    result
}

// ── RESTART-CASE ────────────────────────────────────────────────
fn eval_restart_case(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let (restartable_form, clauses) = cp(cdr);
    let base_len = env.restarts.len();
    let captured_frame = freeze_env_frame(&env.frame);
    let mut c = clauses;
    while c.is_cons() {
        let (clause, rest) = cp(c);
        let (name_form, clause_rest) = cp(clause);
        let (params_form, body) = cp(clause_rest);
        env.restarts.push(RestartEntry {
            name: sym_name(name_form).to_uppercase(),
            function: RestartFunction::FunctionForm {
                function_form: arena_cons(
                    resolve_sym("LAMBDA").unwrap_or(NIL),
                    arena_cons(params_form, body),
                ),
                captured_frame: captured_frame.clone(),
            },
            interactive_function: None,
            test_function: None,
            unwind_on_invoke: true,
        });
        c = rest;
    }

    let result = eval_form(restartable_form, env);
    env.restarts.truncate(base_len);
    match result {
        Ok(value) => Ok(value),
        Err(error) => {
            if let Some(name) = restart_invoked_name(&error) {
                return Ok(take_control_value(&format!("RESTART-RESULT:{name}")));
            }
            Err(error)
        }
    }
}

// ── CERROR ───────────────────────────────────────────────────────
fn eval_cerror(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let (_continue_form, rest) = cp(cdr);
    let (msg_form, _) = cp(rest);
    let msg = eval_form(msg_form, env)?;
    let condition = make_simple_error_condition(msg, env)?;

    let base_len = env.restarts.len();
    env.restarts.push(RestartEntry {
        name: "CONTINUE".to_string(),
        function: RestartFunction::ContinueNil,
        interactive_function: None,
        test_function: None,
        unwind_on_invoke: true,
    });

    let result = signal_condition_object(condition, env);
    env.restarts.truncate(base_len);
    match result {
        Ok(_) => Err(BlissError::Internal(format!("ERROR: {}", val_as_str(msg)))),
        Err(error) => {
            if restart_invoked_name(&error).as_deref() == Some("CONTINUE") {
                return Ok(NIL);
            }
            Err(error)
        }
    }
}

// ── WITH-OPEN-FILE ──────────────────────────────────────────────
fn eval_with_open_file(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let (binding, body) = cp(cdr);
    let (var_form, rest) = cp(binding);
    let (path_form, opts_rest) = cp(rest);

    let var_name = sym_name(var_form);
    let path_val = eval_form(path_form, env)?;
    let path = val_as_str(path_val);

    // Check sandbox mode
    if env.sandbox {
        return Err(BlissError::SandboxViolation(format!(
            "File access denied in sandbox mode: {}",
            path
        )));
    }

    // Parse options
    let opts = list_to_vec(opts_rest);
    let mut direction = StreamDirection::Input;
    let mut i = 0;
    while i < opts.len() {
        let opt_name = sym_name(opts[i]);
        let opt_bare = opt_name
            .trim_start_matches("KEYWORD:")
            .trim_start_matches(':');
        if opt_bare == "DIRECTION" {
            if i + 1 < opts.len() {
                let dir = sym_name(opts[i + 1]);
                let dir_bare = dir.trim_start_matches("KEYWORD:").trim_start_matches(':');
                if dir_bare == "OUTPUT" {
                    direction = StreamDirection::Output;
                }
                i += 2;
            } else {
                i += 1;
            }
        } else {
            i += 1;
        }
    }

    // Open the file and create a stream
    let stream_val = open_stream(&path, direction)?;

    let mut child_env = env.child();
    child_env.define_local(&var_name, stream_val);

    // Evaluate body, then close the stream (unwind-protect style)
    let result = eval_progn(body, &mut child_env);
    close_stream(stream_val)?;
    result
}

// ── DEFPACKAGE ──────────────────────────────────────────────────
fn eval_defpackage(cdr: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let (name_form, opts) = cp(cdr);
    let name_val = eval_form(name_form, env)?;
    let pkg_name = val_as_str(name_val)
        .trim_start_matches("KEYWORD:")
        .trim_start_matches(':')
        .to_uppercase();

    let mut exports = Vec::new();
    let mut uses = Vec::new();

    let mut c = opts;
    while c.is_cons() {
        let (opt, rest) = cp(c);
        if opt.is_cons() {
            let (key, val_list) = cp(opt);
            let key_name = sym_name(key);
            let key_bare = key_name
                .trim_start_matches("KEYWORD:")
                .trim_start_matches(':');
            if key_bare == "USE" {
                let mut vc = val_list;
                while vc.is_cons() {
                    let (v, vr) = cp(vc);
                    uses.push(
                        sym_name(v)
                            .trim_start_matches("KEYWORD:")
                            .trim_start_matches(':')
                            .to_uppercase(),
                    );
                    vc = vr;
                }
            } else if key_bare == "EXPORT" {
                let mut vc = val_list;
                while vc.is_cons() {
                    let (v, vr) = cp(vc);
                    exports.push(
                        sym_name(v)
                            .trim_start_matches("KEYWORD:")
                            .trim_start_matches(':')
                            .to_uppercase(),
                    );
                    vc = vr;
                }
            }
        }
        c = rest;
    }

    Rc::make_mut(&mut env.packages).insert(
        pkg_name.clone(),
        PackageDef {
            name: pkg_name.clone(),
            exports,
            uses,
            symbols: HashMap::new(),
        },
    );
    reader::register_package(&pkg_name);

    Ok(T)
}

// ── FORMAT ───────────────────────────────────────────────────────
fn eval_format(args: BlissVal, env: &mut Env) -> Result<BlissVal, BlissError> {
    let (df, r) = cp(args);
    let dest = eval_form(df, env)?;
    let (ff, fa) = cp(r);
    let fv = eval_form(ff, env)?;
    let fs = val_as_str(fv);
    let mut av = Vec::new();
    let mut c = fa;
    while c.is_cons() {
        let (af, r2) = cp(c);
        av.push(eval_form(af, env)?);
        c = r2;
    }
    bliss_stdlib::format(dest, &fs, &av)
}

fn val_as_str(val: BlissVal) -> String {
    if let Some(s) = bliss_stdlib::registered_string(val) {
        return s;
    }
    if val.is_heap_object() {
        unsafe {
            let p = val.as_ptr();
            let h = *(p as *const ObjectHeader);
            if h.type_id() == type_id::SIMPLE_BASE_STRING {
                let len = *(p.add(8) as *const u64) as usize;
                let data = std::slice::from_raw_parts(p.add(16), len);
                if let Ok(s) = std::str::from_utf8(data) {
                    return s.to_string();
                }
            }
        }
    }
    if val.is_symbol() {
        let name = sym_name(val);
        // For symbols used as keyword args, strip package prefix
        return name;
    }
    format_val(val)
}

fn vals_equal(a: BlissVal, b: BlissVal) -> bool {
    if a == b {
        return true;
    }
    if a.is_fixnum() && b.is_fixnum() {
        return a.as_fixnum() == b.as_fixnum();
    }
    if a.is_single_float() && b.is_single_float() {
        return a.as_single_float() == b.as_single_float();
    }
    if a.is_string() && b.is_string() {
        return val_as_str(a) == val_as_str(b);
    }
    if a.is_cons() && b.is_cons() {
        let (a_car, a_cdr) = cp(a);
        let (b_car, b_cdr) = cp(b);
        return vals_equal(a_car, b_car) && vals_equal(a_cdr, b_cdr);
    }
    false
}

// ── CLI driver ─────────────────────────────────────────────────────
pub fn run(args: &[String]) -> Result<i32, BlissError> {
    let ca = CliArgs::parse(args)?;
    if ca.help {
        print_help();
        return Ok(0);
    }
    if ca.version {
        print_version();
        return Ok(0);
    }

    let mut env = Env::new(ca.sandbox);

    // Set *command-line-args* (issue #4)
    if !ca.cl_args.is_empty() {
        let args_list: Vec<BlissVal> = ca.cl_args.iter().map(|s| arena_str(s)).collect();
        let args_val = vec_to_list(&args_list);
        env.define_local("*COMMAND-LINE-ARGS*", args_val);
    } else {
        env.define_local("*COMMAND-LINE-ARGS*", NIL);
    }

    // Load the Lisp bootstrap prelude. It defines standard-CL forms (defvar,
    // push, incf, ...) that are part of the language, not an optional feature,
    // so it loads unconditionally — before any init file, --eval, --load, or
    // script — for every mode. `--no-bootstrap` opts out (raw evaluator);
    // `--bootstrap` is now the default and kept only for compatibility. A
    // failure here is fatal: the prelude is core.
    if !ca.no_bootstrap {
        let contents = boot_prelude_source()?;
        read_eval_all_env(&contents, &mut env)?;
    }

    // Load init file unless --no-init (issue #8)
    if !ca.no_init && ca.eval.is_none() && ca.load.is_none() && ca.script.is_none() {
        // Try to load init file
        if let Ok(init_path) = std::env::var("BLISS_INIT_FILE") {
            if let Ok(contents) = std::fs::read_to_string(&init_path) {
                let _ = read_eval_all_env(&contents, &mut env);
            }
        } else {
            // Try ~/.blissrc
            if let Ok(home) = std::env::var("HOME") {
                let init_path = format!("{}/.blissrc", home);
                if let Ok(contents) = std::fs::read_to_string(&init_path) {
                    let _ = read_eval_all_env(&contents, &mut env);
                }
            }
        }
    }

    // Load image if specified (issue #8)
    if let Some(ref image_path) = ca.image {
        if let Ok(contents) = std::fs::read_to_string(image_path) {
            read_eval_all_env(&contents, &mut env)?;
        }
    }

    if let Some(ref expr) = ca.eval {
        return run_eval_env(expr, &mut env);
    }
    if let Some(ref path) = ca.load {
        return run_load_env(path, &mut env);
    }
    if let Some(ref script) = ca.script {
        return run_script_env(script, &mut env);
    }
    run_repl_env(&mut env)
}

fn run_eval_env(expr: &str, env: &mut Env) -> Result<i32, BlissError> {
    match with_eval_context(env, EvalContext::Eval, |env| read_eval_all_env(expr, env)) {
        Ok(result) => {
            println!("{}", format_val(result));
            // When the top-level form yielded multiple values, echo the
            // secondary values too (one per line). env.mv holds the full value
            // list only when the last form actually produced multiple values;
            // guard on mv[0] == result so a stale channel from a nested form
            // does not leak extra output.
            let mv = env.mv.clone();
            if mv.len() > 1 && mv[0] == result {
                for v in &mv[1..] {
                    println!("{}", format_val(*v));
                }
            }
            Ok(0)
        }
        Err(e) => {
            eprintln!("ERROR: {}", describe_err(&e));
            Err(e)
        }
    }
}

/// Render an error for display, resolving symbol indices to their names so
/// messages read `undefined function: FOO` instead of `... Symbol(147)`.
fn describe_err(e: &BlissError) -> String {
    match e {
        BlissError::UndefinedFunction(s) => format!("undefined function: {}", sym_name(*s)),
        BlissError::UnboundVariable(s) => format!("unbound variable: {}", sym_name(*s)),
        _ => format!("{}", e),
    }
}

fn run_load_env(path: &str, env: &mut Env) -> Result<i32, BlissError> {
    match load_path_into_env(path, env) {
        Ok(_) => Ok(0),
        Err(e) => {
            eprintln!("ERROR: {}", describe_err(&e));
            Err(e)
        }
    }
}

fn run_script_env(path: &str, env: &mut Env) -> Result<i32, BlissError> {
    let contents = std::fs::read_to_string(path)
        .map_err(|e| BlissError::FileError(format!("cannot read {}: {}", path, e)))?;
    match read_eval_all_env(&contents, env) {
        Ok(_) => Ok(0),
        Err(e) => {
            eprintln!("ERROR: {}", describe_err(&e));
            Err(e)
        }
    }
}

// Keep standalone versions for backward compatibility
#[allow(dead_code)]
fn run_eval(expr: &str) -> Result<i32, BlissError> {
    let mut env = Env::new(false);
    run_eval_env(expr, &mut env)
}

#[allow(dead_code)]
fn run_load(path: &str) -> Result<i32, BlissError> {
    let mut env = Env::new(false);
    run_load_env(path, &mut env)
}

#[allow(dead_code)]
fn run_script(path: &str) -> Result<i32, BlissError> {
    let mut env = Env::new(false);
    run_script_env(path, &mut env)
}

pub fn help_text() -> &'static str {
    concat!(
        "Usage: bliss [OPTIONS] [SCRIPT] [-- CL-ARGS...]\n",
        "\n",
        "Bliss Common Lisp\n",
        "\n",
        "Options:\n",
        "  --help               Print this help message and exit\n",
        "  --version            Print version information and exit\n",
        "  --eval, -e EXPR      Evaluate EXPR and exit\n",
        "  --load FILE          Load FILE and exit\n",
        "  --image FILE         Path to the boot image\n",
        "  --no-image           Start without loading an image\n",
        "  --bootstrap          Deprecated; the prelude now loads by default\n",
        "  --no-bootstrap       Skip the bootstrap prelude (raw evaluator)\n",
        "  --workers N          Number of worker threads\n",
        "  --heap-size SIZE     Heap size (e.g. 512M, 1G)\n",
        "  --tlab-size SIZE     Per-thread TLAB size\n",
        "  --nursery-size SIZE  Nursery size\n",
        "  --stack-size SIZE    CL stack size per green thread\n",
        "  --gc-log FILE        Write GC logs to FILE\n",
        "  --jit-dump           Emit jitdump metadata\n",
        "  --log-level LEVEL    Set log level (error|warn|info|debug|trace)\n",
        "  --sandbox            Enable sandbox mode\n",
        "  --no-init            Skip loading the init file\n",
        "\n",
        "Arguments after -- are passed through to CL as *command-line-args*.\n",
    )
}

pub fn print_help() {
    print!("{}", help_text());
}

pub fn print_version() {
    println!("bliss {}", env!("CARGO_PKG_VERSION"));
}

// ── REPL driver ────────────────────────────────────────────────────
pub fn run_repl() -> Result<i32, BlissError> {
    let mut env = Env::new(false);
    run_repl_env(&mut env)
}

fn run_repl_env(env: &mut Env) -> Result<i32, BlissError> {
    let _config = ReplConfig::default();
    println!("Bliss Common Lisp {}", env!("CARGO_PKG_VERSION"));
    println!("Type (quit) to exit.");
    println!();
    // Promote initial allocations (env setup, image load) to permanent
    ARENA.with(|a| a.borrow_mut().promote_all());
    let stdin = std::io::stdin();
    let mut input = String::new();
    // Debugger state (nesting level, history) carried across debugger entries (A6.02).
    let mut repl_state = bliss_stdlib::ReplState::new();
    loop {
        // Prompt reflects the current package, e.g. `CL-USER> ` (R6.08).
        eprint!("{}> ", prompt_package_name(&env.current_package));
        input.clear();
        match stdin.read_line(&mut input) {
            Ok(0) => {
                println!();
                return Ok(0);
            }
            Ok(_) => {
                let trimmed = input.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if trimmed == "(quit)" || trimmed == "(exit)" {
                    return Ok(0);
                }
                match read_eval_all_env(trimmed, env) {
                    Ok(result) => {
                        println!("{}", format_val(result));
                        // Promote allocations from this eval (they may be stored in env)
                        ARENA.with(|a| a.borrow_mut().promote_all());
                    }
                    Err(e) => {
                        eprintln!("ERROR: {}", describe_err(&e));
                        // Enter the full devtools debugger (R6.13, A6.02): supports
                        // step/next/out/continue, backtrace, frame, eval, restart, and
                        // help. It runs its own read loop and returns once the user
                        // continues, aborts, or requests stepping. In non-interactive
                        // sessions (piped stdin/scripts) it returns immediately so the
                        // REPL keeps consuming input rather than blocking.
                        let condition = arena_str(&format!("{}", e));
                        let _ = bliss_stdlib::invoke_debugger_ui(condition, &mut repl_state);
                        // On error, temporary allocations can be freed
                        ARENA.with(|a| {
                            let mut arena = a.borrow_mut();
                            if arena.should_compact() {
                                arena.compact();
                            }
                        });
                    }
                }
            }
            Err(e) => return Err(BlissError::Internal(format!("read error: {}", e))),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ReplConfig {
    pub history_file: String,
    pub history_size: usize,
    pub syntax_highlighting: bool,
}
impl Default for ReplConfig {
    fn default() -> Self {
        Self {
            history_file: "~/.bliss/repl-history".into(),
            history_size: 1000,
            syntax_highlighting: true,
        }
    }
}
