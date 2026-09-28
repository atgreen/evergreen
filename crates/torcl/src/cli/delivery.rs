//! Saved-world delivery. Analysis runs without Lisp allocations in a stopped
//! world; its result contains symbol indices, never unrooted heap pointers.
use super::*;
use std::collections::{BTreeMap, VecDeque};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use torcl_rt::bytecode::{BytecodeFunction, Instr};
use torcl_rt::symbols;

fn error(message: impl Into<String>) -> TorclError {
    TorclError::ProgramError(format!("delivery: {}", message.into()))
}

struct Spec {
    entry: String,
    packages: Vec<String>,
    keep: Vec<String>,
    explicit_dynamic_roots: bool,
    specialized: bool,
    runtime_keep: std::collections::BTreeSet<String>,
}

impl Spec {
    fn parse(text: &str) -> Result<Self, TorclError> {
        let mut runtime = None;
        let mut runtime_keep = std::collections::BTreeSet::new();
        let mut entry = None;
        let mut version = None;
        let mut dynamic = None;
        let mut packages = Vec::new();
        let mut keep = Vec::new();
        for (line, text) in text.lines().enumerate() {
            let text = text.trim();
            if text.is_empty() || text.starts_with('#') {
                continue;
            }
            let (key, value) = text
                .split_once('=')
                .ok_or_else(|| error(format!("spec line {}: expected key = value", line + 1)))?;
            let (key, value) = (key.trim(), value.trim());
            if value.is_empty() {
                return Err(error(format!("spec line {}: empty value", line + 1)));
            }
            match key {
                "version" if version.is_none() => version = Some(value),
                "entry" if entry.is_none() => entry = Some(value.to_owned()),
                "dynamic" if dynamic.is_none() => dynamic = Some(value),
                "prune-package" => packages.push(value.to_owned()),
                "keep" => keep.push(value.to_owned()),
                "runtime" if runtime.is_none() => runtime = Some(value),
                "runtime-keep" => {
                    if !crate::runtime_contract::CAPABILITIES.contains(&value) {
                        return Err(error(format!("unknown runtime capability {value}")));
                    }
                    runtime_keep.insert(value.to_owned());
                }
                _ => {
                    return Err(error(format!(
                        "spec line {}: unknown or duplicate key {key}",
                        line + 1
                    )));
                }
            }
        }
        if version != Some("1") {
            return Err(error("spec requires version = 1"));
        }
        let explicit_dynamic_roots = match dynamic.unwrap_or("preserve") {
            "preserve" => false,
            "explicit" => true,
            _ => return Err(error("dynamic must be preserve or explicit")),
        };
        let specialized = match runtime.unwrap_or("full") {
            "full" => false,
            "specialized" => true,
            _ => return Err(error("runtime must be full or specialized")),
        };
        if !specialized && !runtime_keep.is_empty() {
            return Err(error("runtime-keep requires runtime = specialized"));
        }
        Ok(Self {
            specialized,
            runtime_keep,
            entry: entry.ok_or_else(|| error("spec requires entry"))?,
            packages,
            keep,
            explicit_dynamic_roots,
        })
    }
}

fn function_index(name: &str) -> Result<u32, TorclError> {
    let (package, bare) = name
        .split_once("::")
        .ok_or_else(|| error(format!("use PACKAGE::FUNCTION for {name}")))?;
    let package = torcl_stdlib::packages::find_package(package)
        .ok_or_else(|| error(format!("unknown package in {name}")))?;
    let (symbol, _) = torcl_stdlib::packages::find_symbol(bare, package)?
        .ok_or_else(|| error(format!("unknown function {name}")))?;
    let index = symbol
        .symbol_index()
        .ok_or_else(|| error(format!("unknown function {name}")))?;
    if symbols::symbol_function(index).is_none_or(|v| v == torcl_rt::value::UNBOUND) {
        return Err(error(format!("undefined function {name}")));
    }
    Ok(index)
}

fn qualified_name(index: u32) -> String {
    let name = symbols::symbol_name(index).unwrap_or_default();
    let bare = name.rsplit(':').next().unwrap_or(&name);
    let package = symbols::symbol_package(index).and_then(torcl_stdlib::packages::package_name);
    match package {
        Some(package) => format!("{package}::{bare}"),
        None => name,
    }
}

/// Include instruction operands as well as GC-visible constants: CallNamed and
/// LoadFunction store symbol indices as raw integers, not tagged GC references.
pub(super) fn bytecode_references(function: &BytecodeFunction) -> Vec<TorclVal> {
    bytecode_references_with_params(function, true)
}

pub(super) fn bytecode_callable_references(function: &BytecodeFunction) -> Vec<TorclVal> {
    bytecode_references_with_params(function, false)
}

fn bytecode_references_with_params(
    function: &BytecodeFunction,
    include_params: bool,
) -> Vec<TorclVal> {
    let mut refs = function.constants.clone();
    if include_params {
        refs.push(function.params_form);
    }
    refs.extend(function.load_time_values.iter().map(|(_, value)| *value));
    for instruction in &function.code {
        if let Instr::CallNamed { sym, .. }
        | Instr::LoadFunction(sym)
        | Instr::LoadGlobal(sym)
        | Instr::StoreGlobal(sym)
        | Instr::BindSpecial(sym) = instruction
        {
            refs.push(TorclVal::from_symbol_index(*sym));
        }
    }
    for handler in &function.handler_binds {
        refs.extend(handler.bindings.iter().map(|(_, form)| *form));
    }
    for restart in &function.restart_cases {
        for entry in &restart.restarts {
            refs.extend(bytecode_references_with_params(
                &entry.function,
                include_params,
            ));
        }
    }
    for nested in &function.nested_functions {
        refs.extend(bytecode_references_with_params(nested, include_params));
    }
    refs
}

struct Plan {
    unreachable_closures: Vec<u32>,
    capability_roots: std::collections::BTreeSet<String>,
    walker_roots: std::collections::BTreeSet<String>,
    capabilities: std::collections::BTreeSet<String>,
    candidates: BTreeMap<u32, String>,
    retained: BTreeMap<u32, String>,
}

fn analyze(spec: &Spec, entry: u32, keeps: &[u32], env: &mut Env) -> Result<Plan, TorclError> {
    let mut package_names = HashSet::new();
    for name in &spec.packages {
        let package = torcl_stdlib::packages::find_package(name)
            .ok_or_else(|| error(format!("unknown prune-package {name}")))?;
        let canonical = torcl_stdlib::packages::package_name(package).unwrap();
        if canonical == "COMMON-LISP" || canonical == "KEYWORD" || canonical.starts_with("TORCL") {
            return Err(error(format!("cannot prune runtime package {canonical}")));
        }
        package_names.insert(canonical);
    }
    torcl_rt::gc::with_heap_snapshot(|| {
        let mut candidates = BTreeMap::new();
        let mut functions = HashMap::new();
        symbols::for_each_bound_function(|index, _, function| {
            functions.insert(index, function);
            if symbols::symbol_package(index)
                .and_then(torcl_stdlib::packages::package_name)
                .is_some_and(|name| package_names.contains(&name))
            {
                candidates.insert(index, qualified_name(index));
            }
        });
        let indices = candidates.keys().copied().collect();
        let mut exposed_roots = symbols::delivery_roots(&indices);
        let mut roots = Vec::new();
        // SAFETY: this entire analysis runs under with_heap_snapshot and makes
        // no Lisp allocations. The omitted scanner is replaced below.
        unsafe {
            torcl_rt::gc::visit_delivery_host_roots(
                &[
                    bytecode::delivery_root_scanner(),
                    torcl_stdlib::hashtable::delivery_root_scanner(),
                ],
                &mut |slot| roots.push(*slot),
            );
        }
        env.visit_gc_roots(&mut |slot| roots.push(unsafe { *slot }));
        let edges = bytecode::delivery_dependencies();
        let compiled = if spec.specialized {
            bytecode::delivery_walker_dependencies()
        } else {
            HashMap::new()
        };
        for (_, function) in global_bytecode_macros() {
            exposed_roots.extend(bytecode_references(&function.lock().unwrap()));
        }
        for function in LOADED_COMPILER_MACRO_FUNCTIONS
            .lock()
            .unwrap()
            .iter()
            .filter_map(Weak::upgrade)
        {
            exposed_roots.extend(bytecode_references(&function.lock().unwrap()));
        }
        let mut queue = VecDeque::new();
        queue.push_back((
            TorclVal::from_symbol_index(entry),
            "entry point".to_owned(),
            true,
        ));
        for &index in keeps {
            queue.push_back((
                TorclVal::from_symbol_index(index),
                "explicit keep".to_owned(),
                true,
            ));
        }
        if !spec.explicit_dynamic_roots || spec.runtime_keep.contains("dynamic-code") {
            for &index in candidates.keys() {
                queue.push_back((
                    TorclVal::from_symbol_index(index),
                    if spec.explicit_dynamic_roots {
                        "runtime-keep = dynamic-code"
                    } else {
                        "dynamic = preserve"
                    }
                    .to_owned(),
                    true,
                ));
            }
        }
        for value in roots {
            queue.push_back((
                value,
                "persistent data or runtime registry".to_owned(),
                false,
            ));
        }
        for value in exposed_roots {
            queue.push_back((
                value,
                "persistent data or runtime registry".to_owned(),
                true,
            ));
        }
        let mut capability_roots = std::collections::BTreeSet::new();
        let mut walker_roots = std::collections::BTreeSet::new();
        if spec.specialized {
            let compiled_methods: HashSet<_> = METHOD_COMPILED.borrow().keys().copied().collect();
            for (name, methods) in env.methods.borrow().iter() {
                if methods
                    .iter()
                    .any(|method| !compiled_methods.contains(&method.method_id.0))
                {
                    walker_roots.insert(format!("{name}: source generic method"));
                }
            }
        }
        let mut capabilities = spec.runtime_keep.clone();
        if !spec.explicit_dynamic_roots {
            capabilities.extend(
                crate::runtime_contract::CAPABILITIES
                    .iter()
                    .map(|s| (*s).to_owned()),
            );
        }
        let mut retained = BTreeMap::new();
        let mut visited = HashSet::new();
        // Runtime metadata (e.g. a class named VECTOR) is not itself a call
        // to that builtin. Values also exposed through code, Lisp globals or
        // captures must be revisited in that role, regardless of visit order.
        let mut visited_roles = HashSet::new();
        // A function stored under an alias must retain the named definition and
        // its bytecode even if its heap representation has no useful name.
        let mut owners: HashMap<u64, Vec<u32>> = HashMap::new();
        for (&index, function) in &functions {
            owners.entry(function.0).or_default().push(index);
        }
        while let Some((value, reason, callable_data)) = queue.pop_front() {
            if !visited_roles.insert((value.0, callable_data)) {
                continue;
            }
            visited.insert(value.0);
            if spec.specialized {
                if torcl_rt::function::is_interpreted_function(value) {
                    let name = torcl_rt::function::name(value);
                    let reason = if !torcl_rt::function::body(value).is_nil() {
                        Some("source function body")
                    } else {
                        match name.symbol_index().and_then(|index| compiled.get(&index)) {
                            Some(reason) => *reason,
                            None => Some("function without saved bytecode"),
                        }
                    };
                    if let Some(reason) = reason {
                        let name = name
                            .symbol_index()
                            .map(qualified_name)
                            .unwrap_or_else(|| "anonymous function".to_owned());
                        walker_roots.insert(format!("{name}: {reason}"));
                    }
                }
                if value.is_cons() {
                    let head = cp(value).0;
                    if head.is_symbol() && sym_bare_name_rc(head).as_ref() == "CLOSURE" {
                        walker_roots.insert("source closure".to_owned());
                    }
                }
            }
            if spec.specialized && value.is_cons() {
                let head = cp(value).0;
                // Saved data and BFASL constants contain raw lambda lists, not
                // QUOTE syntax. Conservatively keep their evaluator surface.
                if head.is_symbol() && sym_bare_name_rc(head).as_ref() == "LAMBDA" {
                    if capabilities.insert("dynamic-code".into()) {
                        for &candidate in candidates.keys() {
                            queue.push_back((
                                TorclVal::from_symbol_index(candidate),
                                "reachable source lambda".into(),
                                true,
                            ));
                        }
                    }
                    capability_roots.insert("source lambda".into());
                }
            }
            if let Some(index) = value.symbol_index() {
                if spec.specialized && !is_keyword_arg(value) && value != NIL && value != T {
                    if let Some(Some(reason)) = compiled.get(&index) {
                        walker_roots.insert(format!("{}: {reason}", qualified_name(index)));
                    }
                    // Bound functions are checked through their actual object
                    // and code, including aliases. Unbound runtime names can
                    // designate native builtins through FUNCALL or saved data.
                    if callable_data && !functions.contains_key(&index) {
                        let name = symbols::symbol_name(index).unwrap_or_default();
                        let runtime_symbol = is_builtin_function(&name)
                            || name.starts_with("TORCL")
                            || symbols::symbol_package(index)
                                .and_then(torcl_stdlib::packages::package_name)
                                .is_some_and(|package| package == "COMMON-LISP");
                        if runtime_symbol && !builtin_without_source_evaluation(&name) {
                            walker_roots.insert(format!(
                                "{}: builtin evaluator dependency",
                                qualified_name(index)
                            ));
                        }
                    }
                }
                if !is_keyword_arg(value)
                    && symbols::symbol_name(index).is_some_and(|name| {
                        crate::runtime_contract::opens_code_world(
                            name.rsplit(':').next().unwrap_or(&name),
                        )
                    })
                {
                    if capabilities.insert("dynamic-code".into()) {
                        for &candidate in candidates.keys() {
                            queue.push_back((
                                TorclVal::from_symbol_index(candidate),
                                format!(
                                    "reachable {} can invoke arbitrary code",
                                    qualified_name(index)
                                ),
                                true,
                            ));
                        }
                    }
                    capability_roots.insert(qualified_name(index));
                }
                if !is_keyword_arg(value)
                    && symbols::symbol_name(index)
                        .is_some_and(|name| name.rsplit(':').next() == Some("DISASSEMBLE"))
                {
                    capabilities.insert("disassembly".to_owned());
                }
                let next_reason = if let Some(name) = candidates.get(&index) {
                    retained.insert(index, reason.clone());
                    format!("{reason} -> {name}")
                } else {
                    reason.clone()
                };
                if let Some(function) = functions.get(&index) {
                    queue.push_back((*function, next_reason.clone(), true));
                }
                if let Some(refs) = edges.get(&index) {
                    for &(value, callable_data) in refs {
                        queue.push_back((value, next_reason.clone(), callable_data));
                    }
                }
            }
            if let Some(indices) = owners.get(&value.0) {
                for &index in indices {
                    queue.push_back((TorclVal::from_symbol_index(index), reason.clone(), true));
                }
            }
            if torcl_stdlib::hashtable::hash_table_p(value) {
                // Entries are off-heap. Package membership tables are not roots;
                // tables reached through application data retain both halves,
                // conservatively including weak entries.
                for (key, child) in torcl_stdlib::hashtable::hash_table_entries(value)
                    .expect("hash_table_p recognized a live table")
                {
                    queue.push_back((key, reason.clone(), callable_data));
                    queue.push_back((child, reason.clone(), callable_data));
                }
            }
            // SAFETY: values came from live roots, registered bodies, or the
            // GC's precise field visitor, all under the same stopped world.
            unsafe {
                // A callable's name and lambda list are metadata. Executable
                // operands/defaults are classified by its registered bytecode;
                // source bodies have already retained the walker above.
                let child_callable_data =
                    callable_data && !torcl_rt::function::is_interpreted_function(value);
                torcl_rt::gc::visit_delivery_references(value, &mut |child| {
                    queue.push_back((child, reason.clone(), child_callable_data))
                });
            }
        }
        if !walker_roots.is_empty() {
            capabilities.insert("tree-walker".into());
        }
        crate::runtime_contract::close_capabilities(&mut capabilities);
        let mut unreachable_closures: Vec<_> = edges
            .keys()
            .copied()
            .filter(|&symbol| {
                symbols::is_uninterned(symbol)
                    && !visited.contains(&TorclVal::from_symbol_index(symbol).0)
            })
            .collect();
        unreachable_closures.sort_unstable();
        Plan {
            unreachable_closures,
            capability_roots,
            walker_roots,
            capabilities,
            candidates,
            retained,
        }
    })
}

/// Audited native entry points with complete evaluated-argument dispatch.
/// This is an implementation dependency catalog, not a delivery allowlist:
/// unknown handlers retain the walker until their source dependencies are
/// removed. Lisp callbacks remain edges through the saved object/code graph.
fn builtin_without_source_evaluation(name: &str) -> bool {
    matches!(
        name,
        "+" | "-"
            | "*"
            | "/"
            | "1+"
            | "1-"
            | "="
            | "/="
            | "<"
            | ">"
            | "<="
            | ">="
            | "CAR"
            | "FIRST"
            | "CDR"
            | "REST"
            | "CONS"
            | "CONSP"
            | "ATOM"
            | "LISTP"
            | "NULL"
            | "NOT"
            | "APPEND"
            | "REVERSE"
            | "ENDP"
            | "EQ"
            | "VALUES"
            | "VALUES-LIST"
            | "FUNCALL"
            | "APPLY"
            | "WRITE-LINE"
    )
}

pub(super) fn run(args: &CliArgs, env: &mut Env) -> Result<i32, TorclError> {
    torcl_rt::rooted_ref!(_env_root = env);
    let input = Path::new(args.image.as_ref().unwrap());
    let spec_path = Path::new(args.deliver.as_ref().unwrap());
    let output = Path::new(args.output.as_ref().unwrap());
    let manifest_path = PathBuf::from(format!("{}.manifest", output.display()));
    for destination in [output, manifest_path.as_path()] {
        if destination.is_dir() {
            return Err(error(format!(
                "output is a directory: {}",
                destination.display()
            )));
        }
        for source in [input, spec_path] {
            if std::fs::canonicalize(destination)
                .ok()
                .is_some_and(|p| Some(p) == std::fs::canonicalize(source).ok())
            {
                return Err(error(
                    "output and manifest must not replace the input image or specification",
                ));
            }
        }
    }
    let text = std::fs::read_to_string(spec_path)
        .map_err(|e| error(format!("read specification: {e}")))?;
    let spec = Spec::parse(&text)?;
    let entry = function_index(&spec.entry)?;
    let keeps = spec
        .keep
        .iter()
        .map(|name| function_index(name))
        .collect::<Result<Vec<_>, _>>()?;
    // This process is disposable. Override the input entry without invoking it;
    // the original on-disk image is never written.
    let top = symbols::intern(IMAGE_TOPLEVEL_VAR);
    symbols::set_symbol_value(top, TorclVal::from_symbol_index(entry));
    torcl_stdlib::pathnames::clear_delivery_string_caches()?;
    let plan = analyze(&spec, entry, &keeps, env)?;
    let mut selected = native_runtime::contract();
    if !spec.specialized
        && crate::runtime_contract::CAPABILITIES
            .iter()
            .any(|name| !selected.capabilities.contains(*name))
    {
        return Err(error("runtime = full requires a full delivery driver"));
    }
    if spec.specialized {
        selected.capabilities = plan.capabilities.clone();
    }
    native_runtime::contract()
        .accepts(&selected)
        .map_err(error)?;
    let input_size = std::fs::metadata(input)
        .map_err(|e| error(e.to_string()))?
        .len();
    let mut header = [0u8; 12];
    std::fs::File::open(input)
        .and_then(|mut file| file.read_exact(&mut header))
        .map_err(|e| error(e.to_string()))?;
    // The core was validated by load_core_image_bytes before delivery started.
    let image_version = u32::from_ne_bytes(header[8..12].try_into().unwrap());
    let mut report = format!(
        "torcl-delivery-manifest = 1\nruntime = {}\ntorcl-version = {}\nplatform-tag = {}\ninput-bytes = {input_size}\nentry = {}\ndynamic = {}\n",
        if spec.specialized {
            "specialized"
        } else {
            "full"
        },
        env!("CARGO_PKG_VERSION"),
        torcl_rt::image::current_platform_tag(),
        spec.entry,
        if spec.explicit_dynamic_roots {
            "explicit"
        } else {
            "preserve"
        }
    );
    report.push_str(&format!(
        "native-contract-begin\n{}native-contract-end\n",
        selected.encode()
    ));
    for root in &plan.capability_roots {
        report.push_str(&format!(
            "native-root = {root} -> dynamic-code -> all native capabilities\n"
        ));
    }
    for root in &plan.walker_roots {
        report.push_str(&format!("native-root = {root} -> tree-walker\n"));
    }
    report.push_str(&format!("input-image-format = {image_version}\n"));
    report.push_str(&format!(
        "private-code-removed = {}\n",
        plan.unreachable_closures.len()
    ));
    for package in &spec.packages {
        report.push_str(&format!("prune-package = {package}\n"));
    }
    for capability in &spec.runtime_keep {
        report.push_str(&format!("explicit-native-root = {capability}\n"));
    }
    for name in &spec.keep {
        report.push_str(&format!("explicit-keep = {name}\n"));
    }
    let mut rows: Vec<_> = plan.candidates.iter().collect();
    rows.sort_by(|a, b| a.1.cmp(b.1));
    for (index, name) in rows {
        if let Some(reason) = plan.retained.get(index) {
            report.push_str(&format!("keep {name}: {reason}\n"));
        } else {
            report.push_str(&format!(
                "remove {name}: unreachable within declared delivery policy\n"
            ));
        }
    }
    if args.dry_run {
        print!("{report}");
        return Ok(0);
    }
    // Reserve both outputs before changing the restored world. A failed build
    // leaves any previous executable untouched.
    let executable_stage = TemporaryFile::new(output).map_err(|e| error(e.to_string()))?;
    let manifest_stage = TemporaryFile::new(&manifest_path).map_err(|e| error(e.to_string()))?;
    let mut runtime = if spec.specialized {
        eprintln!(";; building matching native runtime (cached Cargo release build)");
        native_runtime::build(args.runtime_source.as_deref(), &selected)?
    } else {
        current_runtime_bytes().map_err(|e| error(e.to_string()))?
    };
    remove_embedded_images(&mut runtime).map_err(|e| error(e.to_string()))?;
    let runtime_bytes = runtime.len();
    for &index in plan
        .candidates
        .keys()
        .filter(|index| !plan.retained.contains_key(index))
    {
        symbols::set_symbol_function(index, torcl_rt::value::UNBOUND);
        bytecode::clear_lazy_state(index);
        bytecode::clear_closure_env(index);
    }
    for &symbol in &plan.unreachable_closures {
        bytecode::remove_delivery_closure(symbol);
    }
    native_runtime::with_save_contract(&selected, || {
        save_core(
            executable_stage
                .path
                .to_str()
                .ok_or_else(|| error("output path is not UTF-8"))?,
            false,
            true,
            env,
        )
    })?;
    let core = std::fs::read(&executable_stage.path).map_err(|e| error(e.to_string()))?;
    report.push_str(&format!(
        "native-bytes = {runtime_bytes}\nimage-bytes = {}\n",
        core.len()
    ));
    runtime.extend_from_slice(&core);
    runtime.extend_from_slice(EXE_IMAGE_MAGIC);
    runtime.extend_from_slice(&(core.len() as u64).to_le_bytes());
    write_atomic(&executable_stage.path, &runtime, true).map_err(|e| error(e.to_string()))?;
    report.push_str(&format!(
        "output-bytes = {}\n",
        std::fs::metadata(&executable_stage.path)
            .map_err(|e| error(e.to_string()))?
            .len()
    ));
    write_atomic(&manifest_stage.path, report.as_bytes(), false)
        .map_err(|e| error(e.to_string()))?;
    let previous_manifest = match std::fs::read(&manifest_path) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(error(format!("read previous manifest: {e}"))),
    };
    std::fs::rename(&manifest_stage.path, &manifest_path)
        .map_err(|e| error(format!("publish manifest: {e}")))?;
    if let Err(e) = std::fs::rename(&executable_stage.path, output) {
        let rollback = match previous_manifest {
            Some(bytes) => write_atomic(&manifest_path, &bytes, false),
            None => std::fs::remove_file(&manifest_path),
        };
        if let Err(rollback) = rollback {
            return Err(error(format!(
                "publish executable: {e}; restoring previous manifest also failed: {rollback}"
            )));
        }
        return Err(error(format!("publish executable: {e}")));
    }
    print!("{report}");
    Ok(0)
}

/// Drop all historical payloads, including executables produced by older
/// runtimes which appended new images without removing the preceding one.
pub(super) fn remove_embedded_images(bytes: &mut Vec<u8>) -> io::Result<()> {
    while bytes.len() >= 16 && &bytes[bytes.len() - 16..bytes.len() - 8] == EXE_IMAGE_MAGIC {
        let size = u64::from_le_bytes(bytes[bytes.len() - 8..].try_into().unwrap());
        let size = usize::try_from(size)
            .map_err(|_| io::Error::other("embedded image length overflow"))?;
        let start = bytes
            .len()
            .checked_sub(16)
            .and_then(|n| n.checked_sub(size))
            .ok_or_else(|| io::Error::other("invalid embedded image length"))?;
        if start == 0 || size == 0 {
            return Err(io::Error::other("empty runtime or embedded image"));
        }
        bytes.truncate(start);
    }
    Ok(())
}

pub(super) struct TemporaryFile {
    pub(super) path: PathBuf,
}
impl TemporaryFile {
    pub(super) fn new(destination: &Path) -> io::Result<Self> {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        loop {
            let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let mut name = destination
                .file_name()
                .ok_or_else(|| io::Error::other("output must name a file"))?
                .to_os_string();
            name.push(format!(".delivery-{}-{serial}.tmp", std::process::id()));
            let path = destination.with_file_name(name);
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(_) => return Ok(Self { path }),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
    }
}
impl Drop for TemporaryFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

pub(super) fn write_atomic(path: &Path, bytes: &[u8], executable: bool) -> io::Result<()> {
    let temporary = TemporaryFile::new(path)?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&temporary.path)?;
    file.write_all(bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(if executable {
            0o755
        } else {
            0o644
        }))?;
    }
    #[cfg(not(unix))]
    let _ = executable;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&temporary.path, path)
}
