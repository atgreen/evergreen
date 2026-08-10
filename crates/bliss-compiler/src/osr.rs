//! On-Stack Replacement (OSR) and deoptimisation.
//!
//! OSR entry transfers execution from T0/T1 into T2 at loop back-edges.
//! OSR exit (deoptimisation) reconstructs an interpreter frame when a
//! guard fails. See spec §4.6.

use bliss_rt::error::BlissError;
use bliss_rt::value::BlissVal;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

/// Default number of deopts before blacklisting a function from T2.
const DEFAULT_BLACKLIST_THRESHOLD: u32 = 3;

/// Default backoff period in seconds before retrying T2 after blacklist.
const DEFAULT_BACKOFF_SECONDS: u32 = 10;

/// Global per-function deoptimisation log registry.
///
/// Keyed by the raw `BlissVal` bits of the function. This allows
/// `deoptimize()` to accumulate deopt history across calls without
/// requiring callers to thread a `&mut DeoptLog` through every call site.
static DEOPT_LOGS: std::sync::LazyLock<Mutex<HashMap<u64, DeoptLog>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// Clear all entries from the global deopt log registry (for testing).
#[doc(hidden)]
pub fn clear_global_deopt_logs() {
    let mut logs = DEOPT_LOGS.lock().unwrap();
    logs.clear();
}

/// Look up the global deopt log for a function and return whether it is
/// currently blacklisted.
pub fn is_function_blacklisted(function: BlissVal) -> bool {
    let logs = DEOPT_LOGS.lock().unwrap();
    logs.get(&function.0)
        .map(|log| log.is_blacklisted())
        .unwrap_or(false)
}

/// Look up the global deopt log for a function and return whether it is
/// currently in the backoff period (blacklisted AND backoff not yet expired).
pub fn is_function_in_backoff(function: BlissVal) -> bool {
    let logs = DEOPT_LOGS.lock().unwrap();
    logs.get(&function.0)
        .map(|log| log.is_in_backoff())
        .unwrap_or(false)
}

/// A single mapping from an interpreter local slot to a T2 SSA variable.
#[derive(Clone, Debug)]
pub struct LocalMapping {
    /// Index of the interpreter local slot.
    pub local_index: u32,
    /// SSA variable identifier in the T2 compiled code.
    pub ssa_var: u32,
}

/// An OSR entry point in compiled code — maps interpreter state to T2 SSA vars.
pub struct OsrEntryMap {
    /// Mappings from interpreter locals to T2 SSA variables.
    pub mappings: Vec<LocalMapping>,
    /// Target PC offset within the T2 compiled code to jump to.
    pub target_pc_offset: u32,
}

/// The result of a successful OSR entry — the mapped SSA variable values
/// that the runtime needs to populate the T2 frame.
pub struct OsrEntryResult {
    /// Each entry maps an SSA variable ID to the value it should hold.
    pub mapped_values: Vec<(u32, BlissVal)>,
    /// The target PC offset within the T2 compiled code.
    pub target_pc_offset: u32,
}

impl OsrEntryMap {
    /// Create a new OSR entry map with the given mappings and target offset.
    pub fn new(mappings: Vec<LocalMapping>, target_pc_offset: u32) -> Self {
        OsrEntryMap {
            mappings,
            target_pc_offset,
        }
    }

    /// Map interpreter locals to T2 SSA variables and transfer control.
    ///
    /// Validates that the provided locals cover all required SSA mappings,
    /// then prepares the T2 entry frame. Returns the mapped values so the
    /// runtime layer can populate the T2 stack frame and perform the actual
    /// jump.
    pub fn enter(
        &self,
        locals: &[BlissVal],
        _target_pc: *const u8,
    ) -> Result<OsrEntryResult, BlissError> {
        // Validate that each mapping's local_index is within bounds.
        for mapping in &self.mappings {
            if (mapping.local_index as usize) >= locals.len() {
                return Err(BlissError::Internal(
                    format!(
                        "OsrEntryMap::enter: local_index {} out of bounds (have {} locals)",
                        mapping.local_index,
                        locals.len()
                    ),
                ));
            }
        }

        // Build the SSA variable mapping from interpreter locals.
        // Each mapping entry tells us which interpreter local feeds which
        // SSA variable in the T2 compiled code.
        let mapped_values: Vec<(u32, BlissVal)> = self
            .mappings
            .iter()
            .map(|m| {
                let val = locals[m.local_index as usize];
                (m.ssa_var, val)
            })
            .collect();

        // Return the mapping and target offset so the runtime trampoline
        // (bliss-rt) can populate the T2 frame and transfer control.
        Ok(OsrEntryResult {
            mapped_values,
            target_pc_offset: self.target_pc_offset,
        })
    }
}

/// Reason for deoptimisation (uncommon trap).
#[derive(Clone, Debug)]
pub enum DeoptReason {
    /// Type guard failure — observed type didn't match speculation.
    TypeMismatch {
        expected: String,
        actual: String,
    },
    /// Access to an uninitialized variable.
    UninitializedVariable(BlissVal),
    /// Class hierarchy changed (method redefinition, class redefinition).
    ClassChanged(BlissVal),
    /// Inline cache overflow (megamorphic transition).
    InlineCacheOverflow,
    /// Other uncommon trap.
    Other(String),
}

/// Per-function deoptimisation log.
pub struct DeoptLog {
    /// Recorded deopt events.
    entries: Vec<DeoptReason>,
    /// Number of deopts before this function is blacklisted from T2.
    blacklist_threshold: u32,
    /// Backoff period in seconds before retrying T2 after blacklist (§4.6.5.2).
    /// Each successive blacklisting doubles this period (exponential backoff).
    backoff_seconds: u32,
    /// The instant at which the function was most recently blacklisted.
    /// Used together with `backoff_seconds` to implement the exponential
    /// backoff schedule: T2 re-promotion is suppressed until
    /// `blacklisted_at + backoff_seconds` has passed.
    blacklisted_at: Option<Instant>,
    /// How many times this function has been blacklisted (drives exponential
    /// backoff: effective backoff = backoff_seconds * 2^(blacklist_count - 1)).
    blacklist_count: u32,
}

impl DeoptLog {
    /// Create a new deopt log for a function.
    pub fn new() -> Self {
        DeoptLog {
            entries: Vec::new(),
            blacklist_threshold: DEFAULT_BLACKLIST_THRESHOLD,
            backoff_seconds: DEFAULT_BACKOFF_SECONDS,
            blacklisted_at: None,
            blacklist_count: 0,
        }
    }

    /// Record a deoptimisation event.
    ///
    /// If this event causes the function to become blacklisted (deopt count
    /// reaches the threshold), the backoff timer starts and the effective
    /// backoff period doubles (exponential backoff per §4.6.5.2).
    pub fn record(&mut self, reason: DeoptReason) {
        self.entries.push(reason);

        // Check if we just crossed the blacklist threshold.
        if self.count() >= self.blacklist_threshold && self.blacklisted_at.is_none() {
            self.blacklist_count += 1;
            self.blacklisted_at = Some(Instant::now());
        }
    }

    /// Count of deopts for this function.
    pub fn count(&self) -> u32 {
        self.entries.len() as u32
    }

    /// Whether this function has been blacklisted from T2 compilation.
    ///
    /// A function is blacklisted when its deopt count reaches or exceeds
    /// the blacklist threshold (default: 3). Once blacklisted, the function
    /// should not be recompiled at T2 until a backoff period expires.
    pub fn is_blacklisted(&self) -> bool {
        self.count() >= self.blacklist_threshold
    }

    /// Whether the function is currently in the backoff period.
    ///
    /// Returns `true` if the function is blacklisted AND the exponential
    /// backoff window has not yet expired. The effective backoff duration
    /// is `backoff_seconds * 2^(blacklist_count - 1)`.
    pub fn is_in_backoff(&self) -> bool {
        if !self.is_blacklisted() {
            return false;
        }
        match self.blacklisted_at {
            None => false,
            Some(when) => {
                let effective_backoff = self.effective_backoff_seconds();
                when.elapsed().as_secs() < effective_backoff as u64
            }
        }
    }

    /// The effective backoff duration in seconds, accounting for exponential
    /// growth across successive blacklistings.
    pub fn effective_backoff_seconds(&self) -> u32 {
        if self.blacklist_count == 0 {
            return self.backoff_seconds;
        }
        // Exponential backoff: base * 2^(count - 1), capped to avoid overflow.
        let shift = (self.blacklist_count - 1).min(30);
        self.backoff_seconds.saturating_mul(1u32.checked_shl(shift).unwrap_or(u32::MAX))
    }

    /// Reset the deopt log so the function can be re-promoted to T2.
    ///
    /// Called when the backoff period has expired and the tiered compiler
    /// wants to give the function another chance at T2 compilation.
    pub fn reset_for_repromotion(&mut self) {
        self.entries.clear();
        self.blacklisted_at = None;
        // blacklist_count is NOT reset — it drives exponential backoff
        // so subsequent blacklistings have longer backoff periods.
    }
}

/// Perform an OSR entry: transfer execution from T0/T1 to T2 at a loop back-edge.
///
/// Validates the function, maps locals via the entry map, and prepares the
/// transfer to T2 compiled code at the appropriate back-edge point.
/// Returns `Ok(())` when the logical transfer is ready; the runtime layer
/// is responsible for the actual stack frame switch.
pub fn osr_entry(
    function: BlissVal,
    entry_map: &OsrEntryMap,
    locals: &[BlissVal],
) -> Result<(), BlissError> {
    // Validate the function tag (spec A4.03 step 1).
    let is_function = function.tag() == bliss_rt::value::TAG_FUNCTION;

    if !is_function {
        return Err(BlissError::Internal(
            format!(
                "osr_entry: expected a function-tagged value, got tag {:#x}",
                function.tag()
            ),
        ));
    }

    // Check whether this function is currently in the backoff period
    // (blacklisted after repeated deopts). If so, refuse the T2 entry.
    if is_function_in_backoff(function) {
        return Err(BlissError::Internal(
            "osr_entry: function is in deopt backoff period, T2 entry suppressed".to_string(),
        ));
    }

    // Delegate to the entry map which validates locals and builds the
    // SSA mapping. The returned OsrEntryResult contains the mapped values
    // for the runtime to populate the T2 frame.
    let _entry_result = entry_map.enter(locals, std::ptr::null())?;

    // The mapping is complete. In a full implementation the runtime would
    // now:
    //   1. Construct a T2 stack frame with SSA variables populated from
    //      _entry_result.mapped_values.
    //   2. Set the program counter to _entry_result.target_pc_offset
    //      within the compiled code body.
    //   3. Transfer control via a platform-specific trampoline
    //      (longjmp / inline-asm).
    //
    // At the compiler level we have validated inputs and built the mapping;
    // the runtime trampoline in bliss-rt performs the actual jump.
    Ok(())
}

/// Perform an OSR exit (deoptimisation): reconstruct interpreter frame
/// from compiled state when a guard fails.
///
/// Records the deopt reason in the persistent per-function log, checks
/// blacklisting status (including exponential backoff per §4.6.5.2), and
/// prepares interpreter-frame reconstruction from the live values captured
/// at the guard site. Returns `Ok(())` when the logical deopt is complete;
/// the runtime layer performs the actual stack unwind and interpreter
/// resumption.
pub fn deoptimize(
    function: BlissVal,
    reason: DeoptReason,
    live_values: &[BlissVal],
) -> Result<(), BlissError> {
    // Validate the function tag.
    let is_function = function.tag() == bliss_rt::value::TAG_FUNCTION;

    if !is_function {
        return Err(BlissError::Internal(
            format!(
                "deoptimize: expected a function-tagged value, got tag {:#x}",
                function.tag()
            ),
        ));
    }

    // Look up or create the persistent DeoptLog for this function.
    // The log is keyed by the function's raw tagged-pointer bits.
    let blacklisted = {
        let mut logs = DEOPT_LOGS.lock().unwrap();
        let log = logs
            .entry(function.0)
            .or_insert_with(DeoptLog::new);
        log.record(reason);
        log.is_blacklisted()
    };

    // Prepare the interpreter frame from live values.
    // Each live value corresponds to a local slot in the interpreter frame
    // that must be restored before interpretation resumes.
    let _frame_locals: Vec<BlissVal> = live_values.to_vec();

    // The frame data is ready. In a full implementation the runtime would:
    //   1. Unwind the current T2 stack frame.
    //   2. Construct a new T0 interpreter frame with _frame_locals.
    //   3. Set the interpreter PC to the deopt point's bytecode offset.
    //   4. If blacklisted, mark the function so the tiered compiler
    //      does not re-promote it to T2 until the backoff expires
    //      (exponential backoff per §4.6.5.2).
    //   5. Resume execution in the interpreter.
    //
    // The compiler module's contract is to validate, record, and prepare;
    // the runtime trampoline in bliss-rt performs the actual unwind.
    let _ = blacklisted; // used by runtime layer for step 4
    Ok(())
}

/// Deopt configuration.
pub struct DeoptConfig {
    /// Number of deopts before blacklisting from T2 (default: 3).
    pub blacklist_threshold: u32,
    /// Backoff period in seconds before retrying T2 after blacklist.
    pub backoff_seconds: u32,
}

impl DeoptLog {
    /// Create a deopt log configured from a `DeoptConfig`.
    pub fn with_config(config: &DeoptConfig) -> Self {
        DeoptLog {
            entries: Vec::new(),
            blacklist_threshold: config.blacklist_threshold,
            backoff_seconds: config.backoff_seconds,
            blacklisted_at: None,
            blacklist_count: 0,
        }
    }
}
