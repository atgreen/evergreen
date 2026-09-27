//! Runtime operations for the public Lisp fiber library in lib/fibers.lisp.
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, Once, OnceLock};
use std::time::{Duration, Instant};
use torcl_rt::thread::{self, FiberId, FiberState};
use torcl_rt::value::{NIL, T};
use torcl_rt::{SchedulerConfig, SchedulerGroup, TorclError, TorclVal};

// Registered fibers remain observable until explicitly joined. A join transfers
// results to the Lisp object's slots, then removes both runtime registrations.
static FIBERS: LazyLock<Mutex<HashMap<FiberId, TorclVal>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static GROUPS: LazyLock<Mutex<HashMap<u64, Arc<SchedulerGroup>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static PACKAGES: LazyLock<Mutex<HashMap<FiberId, crate::packages::PackageContext>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub fn activate_packages() -> Option<crate::packages::ActivePackageRegistryGuard> {
    let id = thread::current_fiber_id()?;
    let context = PACKAGES.lock().unwrap().get(&id)?.clone();
    Some(context.activate())
}

static IDLE_ENTRIES: LazyLock<Mutex<HashMap<u64, TorclVal>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static NEXT_GROUP: AtomicU64 = AtomicU64::new(1);

fn install_roots() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        torcl_rt::gc::register_root_scanner(|visit| {
            for value in IDLE_ENTRIES.lock().unwrap().values_mut() {
                visit(value);
            }
            for value in FIBERS.lock().unwrap().values_mut() {
                visit(value);
            }
        })
    });
}

fn id(value: TorclVal) -> Result<u64, TorclError> {
    if value.is_fixnum() && value.as_fixnum() > 0 {
        Ok(value.as_fixnum() as u64)
    } else {
        Err(TorclError::TypeError {
            datum: value,
            expected: "positive runtime identity".into(),
        })
    }
}

fn group(value: TorclVal) -> Result<Arc<SchedulerGroup>, TorclError> {
    GROUPS
        .lock()
        .unwrap()
        .get(&id(value)?)
        .cloned()
        .ok_or_else(|| TorclError::ProgramError("scheduler group is no longer active".into()))
}

fn timeout(value: TorclVal) -> Result<Option<Duration>, TorclError> {
    if value == NIL {
        return Ok(None);
    }
    if value.is_double_float() {
        if let Ok(duration) = Duration::try_from_secs_f64(value.as_double_float()) {
            return Ok(Some(duration));
        }
    }
    Err(TorclError::TypeError {
        datum: value,
        expected: "finite non-negative double-float timeout or NIL".into(),
    })
}

fn wait(fiber: FiberId, timeout: Option<Duration>) -> Result<bool, TorclError> {
    let start = Instant::now();
    let done = || match thread::fiber_state(fiber) {
        Some(FiberState::Dead) => Ok(true),
        Some(_) => Ok(false),
        None => Err(TorclError::ProgramError("unknown fiber".into())),
    };
    if done()? {
        return Ok(true);
    }
    if timeout == Some(Duration::ZERO) {
        return Ok(false);
    }
    if thread::current_fiber_id() == Some(fiber) {
        return Err(TorclError::ProgramError(
            "a fiber cannot join itself".into(),
        ));
    }
    let mode = torcl_rt::sync::blocking_mode("FIBER-JOIN")?;
    // SAFETY: the native loop uses only Rust-owned identities and timestamps.
    let _blocked = (mode == torcl_rt::sync::BlockingMode::Native)
        .then(|| unsafe { torcl_rt::safepoint::NativeBlockingScope::enter() });
    loop {
        if done()? {
            return Ok(true);
        }
        let pause = match timeout {
            Some(limit) => match limit.checked_sub(start.elapsed()) {
                Some(left) if !left.is_zero() => left.min(Duration::from_millis(1)),
                _ => return Ok(false),
            },
            None => Duration::from_millis(1),
        };
        match mode {
            torcl_rt::sync::BlockingMode::Native => std::thread::sleep(pause),
            torcl_rt::sync::BlockingMode::Fiber => torcl_rt::sync::fiber_sleep(pause)?,
        }
    }
}

/// Private bridge. Public keyword/condition and result policy belongs to Lisp.
pub fn call(args: &[TorclVal]) -> Result<TorclVal, TorclError> {
    torcl_rt::rooted!(rooted_args = args.to_vec());
    let args = &rooted_args[..];
    install_roots();
    let operation = args
        .first()
        .and_then(|v| v.symbol_index())
        .and_then(torcl_rt::symbols::symbol_name)
        .unwrap_or_default();
    let args = args.get(1..).unwrap_or(&[]);
    match (
        operation.strip_prefix("KEYWORD:").unwrap_or(&operation),
        args,
    ) {
        ("EPOCH", []) => {
            static EPOCH: OnceLock<u64> = OnceLock::new();
            let epoch = EPOCH.get_or_init(|| {
                let time = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos() as u64;
                (time ^ u64::from(std::process::id())) & ((1 << 55) - 1)
            });
            Ok(TorclVal::from_fixnum(*epoch as i64))
        }
        ("MAKE", [entry, size]) => {
            if !cfg!(all(target_arch = "x86_64", any(unix, windows))) {
                return Err(TorclError::ProgramError(
                    "stackful Lisp fibers require x86-64 on Unix or Windows".into(),
                ));
            }
            let context = crate::packages::PackageContext::capture()?;
            let fiber = thread::make_fiber_with_stack_size(
                *entry,
                usize::try_from(id(*size)?)
                    .map_err(|_| TorclError::ProgramError("stack size is too large".into()))?,
            )?;
            PACKAGES.lock().unwrap().insert(fiber, context);
            Ok(TorclVal::from_fixnum(fiber.0 as i64))
        }
        ("REGISTER", [number, object]) => {
            FIBERS
                .lock()
                .unwrap()
                .insert(FiberId(id(*number)?), *object);
            Ok(*object)
        }
        ("CURRENT", []) => Ok(thread::current_fiber_id()
            .and_then(|id| FIBERS.lock().unwrap().get(&id).copied())
            .unwrap_or(NIL)),
        ("ALL", []) => {
            let mut values: Vec<_> = FIBERS.lock().unwrap().values().copied().collect();
            torcl_rt::rooted_ref!(_values = &mut values);
            Ok(crate::sequences::build_simple_vector(&values))
        }
        // Public descriptors survive joining. A removed identity is terminal;
        // observing it during the result handoff must still report DEAD.
        ("STATE", [number]) => Ok(TorclVal::from_fixnum(
            thread::fiber_state(FiberId(id(*number)?)).unwrap_or(FiberState::Dead) as i64,
        )),
        ("CARRIER", [number]) => Ok(thread::fiber_carrier_thread(FiberId(id(*number)?))
            .map(|id| TorclVal::from_fixnum(id.0 as i64))
            .unwrap_or(NIL)),
        ("BACKTRACE", [number, count]) => {
            let count = if count.is_fixnum() && count.as_fixnum() >= 0 {
                count.as_fixnum() as usize
            } else {
                return Err(TorclError::ProgramError(
                    "backtrace count must be nonnegative".into(),
                ));
            };
            let fiber_id = FiberId(id(*number)?);
            let frames = match thread::fiber_backtrace(fiber_id, count) {
                Ok(frames) => frames,
                Err(_) if thread::fiber_state(fiber_id).is_none() => Some(Vec::new()),
                Err(error) => return Err(error),
            };
            let Some(names) = frames else {
                return Ok(NIL);
            };
            torcl_rt::rooted!(values = Vec::<TorclVal>::new());
            for name in names {
                values.push(crate::streams::make_lisp_string(&name));
            }
            Ok(crate::sequences::build_simple_vector(&values))
        }
        ("PIN", [number]) => {
            thread::fiber_pin(FiberId(id(*number)?))?;
            Ok(NIL)
        }
        ("UNPIN", [number]) => {
            thread::fiber_unpin(FiberId(id(*number)?))?;
            Ok(NIL)
        }
        ("CAN-YIELD", [number]) => Ok(if thread::fiber_can_yield(FiberId(id(*number)?))? {
            T
        } else {
            NIL
        }),
        ("YIELD", []) => {
            thread::fiber_yield()?;
            Ok(NIL)
        }
        ("SLEEP", [seconds]) => {
            let duration = timeout(*seconds)?
                .ok_or_else(|| TorclError::ProgramError("sleep needs a duration".into()))?;
            let mode = torcl_rt::sync::blocking_mode("FIBER-SLEEP")?;
            if mode == torcl_rt::sync::BlockingMode::Native {
                // SAFETY: only an owned Duration is accessed while blocked.
                let _blocked = unsafe { torcl_rt::safepoint::NativeBlockingScope::enter() };
                std::thread::sleep(duration);
            } else {
                torcl_rt::sync::fiber_sleep(duration)?;
            }
            Ok(NIL)
        }
        ("WAIT", [number, limit]) => Ok(if wait(FiberId(id(*number)?), timeout(*limit)?)? {
            T
        } else {
            NIL
        }),
        ("JOIN", [number]) => {
            let id = FiberId(id(*number)?);
            let result = thread::join_fiber(id);
            FIBERS.lock().unwrap().remove(&id);
            PACKAGES.lock().unwrap().remove(&id);
            result
        }
        ("START", [count]) => {
            let count = usize::try_from(id(*count)?)
                .map_err(|_| TorclError::ProgramError("carrier count too large".into()))?;
            let group = Arc::new(SchedulerGroup::init(&SchedulerConfig {
                num_workers: count,
            })?);
            let id = NEXT_GROUP.fetch_add(1, Ordering::Relaxed);
            GROUPS.lock().unwrap().insert(id, group);
            Ok(TorclVal::from_fixnum(id as i64))
        }
        ("PROCESSORS", []) => Ok(TorclVal::from_fixnum(
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1) as i64,
        )),
        ("IDLE-HOOK", [owner, entry]) => {
            let owner_id = id(*owner)?;
            let context = crate::packages::PackageContext::capture()?;
            IDLE_ENTRIES.lock().unwrap().insert(owner_id, *entry);
            group(*owner)?.set_idle_hook(Some(Arc::new(move || {
                let entry = IDLE_ENTRIES.lock().unwrap().get(&owner_id).copied();
                if let Some(entry) = entry {
                    torcl_rt::rooted!(entry = entry);
                    let _packages = context.activate();
                    // Lisp wraps the hook to save its condition in the group.
                    let _ = thread::run_entry(*entry);
                }
            })));
            Ok(NIL)
        }
        ("SECONDS", [value]) => {
            timeout(*value)?;
            Ok(*value)
        }
        ("SUBMIT", [owner, number]) => {
            group(*owner)?.submit(FiberId(id(*number)?))?;
            Ok(NIL)
        }
        ("SHUTDOWN", [owner]) => {
            group(*owner)?.shutdown()?;
            GROUPS.lock().unwrap().remove(&id(*owner)?);
            IDLE_ENTRIES.lock().unwrap().remove(&id(*owner)?);
            Ok(NIL)
        }
        ("CARRIERS", [owner]) => {
            let group = group(*owner)?;
            let ids: Vec<_> = group
                .carrier_thread_ids()
                .iter()
                .map(|id| TorclVal::from_fixnum(id.0 as i64))
                .collect();
            Ok(crate::sequences::build_simple_vector(&ids))
        }
        _ => Err(TorclError::ProgramError(format!(
            "invalid fiber operation {operation} with {} arguments",
            args.len()
        ))),
    }
}
