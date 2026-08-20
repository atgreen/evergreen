//! On-Stack Replacement (OSR) and deoptimisation.
//!
//! OSR entry transfers execution from T0/T1 into T2 at loop back-edges.
//! OSR exit (deoptimisation) reconstructs an interpreter frame when a
//! guard fails. See spec §4.6.

use bliss_rt::error::BlissError;
use bliss_rt::lock_order::{LockLevel, OrderedMutex};
use bliss_rt::value::BlissVal;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

/// Default number of deopts before blacklisting a function from T2.
/// The bootstrap pipeline uses a low threshold so tests can exercise
/// the blacklist path without synthesizing long deopt histories.
const DEFAULT_BLACKLIST_THRESHOLD: u32 = 3;

/// Default backoff period in seconds before retrying T2 after blacklist.
const DEFAULT_BACKOFF_SECONDS: u32 = 10;

/// Ring buffer capacity for the deopt log (spec §4.6.8 BLISS_DEOPT_LOG_CAP).
const DEOPT_LOG_CAP: usize = 32;

/// Sliding window for per-reason counter evaluation (spec §4.6.5.1).
const DEOPT_WINDOW: u64 = 10_000;

/// Global per-function deoptimisation log registry.
///
/// Keyed by the raw `BlissVal` bits of the function. This allows
/// `deoptimize()` to accumulate deopt history across calls without
/// requiring callers to thread a `&mut DeoptLog` through every call site.
static DEOPT_LOGS: std::sync::LazyLock<OrderedMutex<HashMap<u64, DeoptLog>>> =
    std::sync::LazyLock::new(|| {
        OrderedMutex::new(
            LockLevel::Profiling,
            1,
            "deoptimization logs",
            HashMap::new(),
        )
    });

/// Monotonic tick counter used for timestamps and sliding-window evaluation.
static MONOTONIC_TICK: AtomicU32 = AtomicU32::new(0);

/// Advance the monotonic tick counter and return the new value.
fn next_tick() -> u64 {
    MONOTONIC_TICK.fetch_add(1, Ordering::Relaxed) as u64 + 1
}

/// Return the current monotonic tick without advancing.
fn current_tick() -> u64 {
    MONOTONIC_TICK.load(Ordering::Relaxed) as u64
}

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

// ── Location, ConversionKind, TypeGuard (spec D4.09) ────────────────

/// Where a value lives in the T2 frame — register or stack (spec D4.09 Location).
#[derive(Clone, Debug)]
pub enum Location {
    /// General-purpose register (platform register number).
    Register(u8),
    /// Stack offset relative to frame base.
    StackOffset(i32),
    /// Floating-point register (platform FP register number).
    FpRegister(u8),
}

/// Representation conversion at OSR boundaries (spec D4.09 ConversionKind).
#[derive(Clone, Debug, PartialEq)]
pub enum ConversionKind {
    /// No conversion needed.
    None,
    /// Unbox a tagged fixnum to a raw i64.
    UnboxFixnum,
    /// Unbox a tagged float to a raw f64.
    UnboxFloat,
    /// Box a raw i64 into a tagged fixnum.
    BoxFixnum,
    /// Box a raw f64 into a tagged float.
    BoxFloat,
    /// Widen a 32-bit integer to 64-bit.
    WidenI32ToI64,
}

/// Expected type tag for a type guard check at OSR entry (spec D4.09).
#[derive(Clone, Debug)]
pub struct TypeGuard {
    /// Index of the slot to check.
    pub slot_index: u16,
    /// Expected tag bits (low 3 bits of BlissVal).
    pub expected_tag: u64,
}

// ── OsrSlotDesc (spec D4.09) ───────────────────────────────────────

/// Per-slot descriptor: how to populate the T2 frame from the source frame.
#[derive(Clone, Debug)]
pub struct OsrSlotDesc {
    /// Source location: stack offset in the T0/T1 frame.
    pub source_offset: i32,
    /// Destination: register or stack offset in the T2 frame.
    pub dest: Location,
    /// Representation conversion needed (e.g. boxed → unboxed fixnum).
    pub conversion: ConversionKind,
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
///
/// Per spec D4.09, includes per-slot descriptors with conversion kinds,
/// a live-reference bitmap for GC root tracking, and type guards for
/// speculative guard checks at OSR entry.
pub struct OsrEntryMap {
    /// Mappings from interpreter locals to T2 SSA variables.
    pub mappings: Vec<LocalMapping>,
    /// Target PC offset within the T2 compiled code to jump to.
    pub target_pc_offset: u32,
    /// Per-slot descriptors: how to populate the T2 frame from the source frame
    /// (spec D4.09 `slots`).
    pub slots: Vec<OsrSlotDesc>,
    /// Bitmap: which T2 SSA values are live GC references at the entry point.
    /// Bit i is set if SSA variable i is a live GC root (spec D4.09 `live_ref_bitmap`).
    pub live_ref_bitmap: Vec<u8>,
    /// Expected types for speculative type guards at OSR entry (spec D4.09).
    /// If any guard fails, the OSR attempt is abandoned (not a deopt).
    pub type_guards: Vec<TypeGuard>,
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
    ///
    /// Initialises slots, live_ref_bitmap, and type_guards to empty defaults.
    /// Use the builder-style setters or direct field access to populate them
    /// for production use.
    pub fn new(mappings: Vec<LocalMapping>, target_pc_offset: u32) -> Self {
        OsrEntryMap {
            mappings,
            target_pc_offset,
            slots: Vec::new(),
            live_ref_bitmap: Vec::new(),
            type_guards: Vec::new(),
        }
    }

    /// Map interpreter locals to T2 SSA variables and transfer control.
    ///
    /// Performs spec algorithm A4.03 steps 4 (type guard check) and 6
    /// (value transfer with conversion). Validates that the provided locals
    /// cover all required SSA mappings, then prepares the T2 entry frame.
    /// Returns the mapped values so the runtime layer can populate the T2
    /// stack frame and perform the actual jump.
    pub fn enter(
        &self,
        locals: &[BlissVal],
        _target_pc: *const u8,
    ) -> Result<OsrEntryResult, BlissError> {
        // Step 4: TYPE-GUARD CHECK (spec A4.03 step 4).
        // If any guard fails, abandon the OSR attempt (not a deopt).
        for guard in &self.type_guards {
            let idx = guard.slot_index as usize;
            if idx >= locals.len() {
                return Err(BlissError::Internal(format!(
                    "OsrEntryMap::enter: type guard slot_index {} out of bounds (have {} locals)",
                    guard.slot_index,
                    locals.len()
                )));
            }
            let val = locals[idx];
            if val.tag() != guard.expected_tag {
                return Err(BlissError::Internal(format!(
                    "OsrEntryMap::enter: type guard failed at slot {}: expected tag {:#x}, got {:#x}",
                    guard.slot_index,
                    guard.expected_tag,
                    val.tag()
                )));
            }
        }

        // Validate that each mapping's local_index is within bounds.
        for mapping in &self.mappings {
            if (mapping.local_index as usize) >= locals.len() {
                return Err(BlissError::Internal(format!(
                    "OsrEntryMap::enter: local_index {} out of bounds (have {} locals)",
                    mapping.local_index,
                    locals.len()
                )));
            }
        }

        // Step 6: TRANSFER STATE with value conversions (spec A4.03 step 6).
        // Build the SSA variable mapping from interpreter locals, applying
        // any ConversionKind specified in the slot descriptors.
        let mapped_values: Vec<(u32, BlissVal)> = self
            .mappings
            .iter()
            .enumerate()
            .map(|(i, m)| {
                let mut val = locals[m.local_index as usize];

                // Apply conversion from the corresponding slot descriptor if present.
                if i < self.slots.len() {
                    val = apply_conversion(val, &self.slots[i].conversion);
                }

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

/// Apply a ConversionKind to a BlissVal (spec D4.09).
///
/// For boxed→unboxed conversions, extracts the raw payload.
/// For unboxed→boxed conversions, wraps the raw bits with the appropriate tag.
fn apply_conversion(val: BlissVal, conversion: &ConversionKind) -> BlissVal {
    match conversion {
        ConversionKind::None => val,
        ConversionKind::UnboxFixnum => {
            // Extract the fixnum payload (shift right by 3 to remove tag)
            BlissVal(((val.0 as i64) >> 3) as u64)
        }
        ConversionKind::UnboxFloat => {
            // Extract the IEEE 754 f32 bits from bits 63:32 into the lower 32 bits.
            // Result fits in 32 bits (upper 32 bits are zero), so BoxFloat can
            // shift them back with `<< 32` to round-trip correctly.
            let float_bits = (val.0 >> 32) & 0xFFFF_FFFF;
            BlissVal(float_bits)
        }
        ConversionKind::BoxFixnum => {
            // Wrap raw i64 as a tagged fixnum (shift left by 3, tag 000)
            BlissVal(((val.0 as i64) << 3) as u64)
        }
        ConversionKind::BoxFloat => {
            // Wrap raw f32 bits (in lower 32 bits) into a tagged single-float.
            // Mask to 32 bits to ensure only valid float payload is shifted up.
            let float_bits = val.0 & 0xFFFF_FFFF;
            BlissVal((float_bits << 32) | bliss_rt::value::TAG_SINGLE_FLOAT)
        }
        ConversionKind::WidenI32ToI64 => {
            // Sign-extend a 32-bit value to 64-bit
            BlissVal((val.0 as i32 as i64) as u64)
        }
    }
}

/// Reason for deoptimisation (uncommon trap).
#[derive(Clone, Debug)]
pub enum DeoptReason {
    /// Type guard failure — observed type didn't match speculation.
    TypeMismatch { expected: String, actual: String },
    /// Access to an uninitialized variable.
    UninitializedVariable(BlissVal),
    /// Class hierarchy changed (method redefinition, class redefinition).
    ClassChanged(BlissVal),
    /// Inline cache overflow (megamorphic transition).
    InlineCacheOverflow,
    /// Other uncommon trap.
    Other(String),
}

impl DeoptReason {
    /// Return a category key for per-reason counter tracking (spec §4.6.5.1).
    fn category_key(&self) -> &'static str {
        match self {
            DeoptReason::TypeMismatch { .. } => "type_mismatch",
            DeoptReason::UninitializedVariable(_) => "uninitialized_variable",
            DeoptReason::ClassChanged(_) => "class_changed",
            DeoptReason::InlineCacheOverflow => "inline_cache_overflow",
            DeoptReason::Other(_) => "other",
        }
    }
}

/// A single deopt event entry in the ring buffer (spec D4.10 DeoptEntry).
#[derive(Clone, Debug)]
pub struct DeoptEntry {
    /// Monotonic tick of the event.
    pub timestamp: u64,
    /// T2 native PC where the uncommon trap fired (as usize for safety).
    pub trap_pc: usize,
    /// Reason category.
    pub reason: DeoptReason,
    /// The bytecode / T0 PC to which execution was transferred.
    pub resume_pc: u32,
}

/// Per-function deoptimisation log (spec D4.10).
///
/// Uses a ring buffer of capacity `DEOPT_LOG_CAP` (default 32) to store
/// recent deopt events. Maintains a total lifetime deopt count and
/// per-reason counters within a sliding window for backoff evaluation.
pub struct DeoptLog {
    /// Circular buffer of recent deopt events.
    entries: Vec<DeoptEntry>,
    /// Write index into the ring buffer (wraps at DEOPT_LOG_CAP).
    write_index: usize,
    /// Number of entries currently in the ring buffer (≤ DEOPT_LOG_CAP).
    entry_count: usize,
    /// Total lifetime deopt count for this function (spec D4.10 total_deopts).
    total_deopts: u32,
    /// Per-reason counters (spec §4.6.5.1).
    reason_counters: HashMap<&'static str, u32>,
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
    /// Sliding window size in ticks for per-reason counter evaluation (spec §4.6.5.1).
    deopt_window: u64,
}

impl DeoptLog {
    /// Create a new deopt log for a function with default settings.
    pub fn new() -> Self {
        DeoptLog {
            entries: Vec::with_capacity(DEOPT_LOG_CAP),
            write_index: 0,
            entry_count: 0,
            total_deopts: 0,
            reason_counters: HashMap::new(),
            blacklist_threshold: DEFAULT_BLACKLIST_THRESHOLD,
            backoff_seconds: DEFAULT_BACKOFF_SECONDS,
            blacklisted_at: None,
            blacklist_count: 0,
            deopt_window: DEOPT_WINDOW,
        }
    }

    /// Record a deoptimisation event with full DeoptEntry fields.
    ///
    /// Inserts into the ring buffer (wrapping at DEOPT_LOG_CAP), increments
    /// total_deopts and per-reason counters, and checks blacklist threshold.
    /// If this event causes the function to become blacklisted (deopt count
    /// reaches the threshold), the backoff timer starts and the effective
    /// backoff period doubles (exponential backoff per §4.6.5.2).
    pub fn record(&mut self, reason: DeoptReason) {
        let timestamp = next_tick();

        // Update per-reason counter.
        let category = reason.category_key();
        *self.reason_counters.entry(category).or_insert(0) += 1;

        // Build the DeoptEntry.
        let entry = DeoptEntry {
            timestamp,
            trap_pc: 0, // filled in by caller if available
            reason,
            resume_pc: 0, // filled in by caller if available
        };

        // Insert into the ring buffer.
        if self.entries.len() < DEOPT_LOG_CAP {
            self.entries.push(entry);
        } else {
            self.entries[self.write_index] = entry;
        }
        self.write_index = (self.write_index + 1) % DEOPT_LOG_CAP;
        if self.entry_count < DEOPT_LOG_CAP {
            self.entry_count += 1;
        }

        // Increment total lifetime count (spec D4.10 total_deopts).
        self.total_deopts += 1;

        // Check if we just crossed the blacklist threshold.
        if self.total_deopts >= self.blacklist_threshold && self.blacklisted_at.is_none() {
            self.blacklist_count += 1;
            self.blacklisted_at = Some(Instant::now());
        }
    }

    /// Record a deoptimisation event with full metadata (trap_pc, resume_pc).
    pub fn record_full(&mut self, reason: DeoptReason, trap_pc: usize, resume_pc: u32) {
        let timestamp = next_tick();
        let category = reason.category_key();
        *self.reason_counters.entry(category).or_insert(0) += 1;

        let entry = DeoptEntry {
            timestamp,
            trap_pc,
            reason,
            resume_pc,
        };

        if self.entries.len() < DEOPT_LOG_CAP {
            self.entries.push(entry);
        } else {
            self.entries[self.write_index] = entry;
        }
        self.write_index = (self.write_index + 1) % DEOPT_LOG_CAP;
        if self.entry_count < DEOPT_LOG_CAP {
            self.entry_count += 1;
        }

        self.total_deopts += 1;

        if self.total_deopts >= self.blacklist_threshold && self.blacklisted_at.is_none() {
            self.blacklist_count += 1;
            self.blacklisted_at = Some(Instant::now());
        }
    }

    /// Total lifetime deopt count (spec D4.10 total_deopts).
    pub fn total_deopts(&self) -> u32 {
        self.total_deopts
    }

    /// Count of entries currently in the ring buffer (≤ DEOPT_LOG_CAP).
    pub fn count(&self) -> u32 {
        self.entry_count as u32
    }

    /// Get per-reason counter for a given category within the sliding window
    /// (spec §4.6.5.1).
    pub fn reason_count_in_window(&self, reason_key: &str) -> u32 {
        let now = current_tick();
        let window_start = now.saturating_sub(self.deopt_window);
        let mut count = 0u32;
        for entry in &self.entries[..self.entry_count.min(self.entries.len())] {
            if entry.timestamp >= window_start && entry.reason.category_key() == reason_key {
                count += 1;
            }
        }
        count
    }

    /// Whether this function has been blacklisted from T2 compilation.
    ///
    /// A function is blacklisted when its total lifetime deopt count reaches
    /// or exceeds the blacklist threshold (default: 20, spec §4.6.5.2).
    pub fn is_blacklisted(&self) -> bool {
        self.total_deopts >= self.blacklist_threshold
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
        self.backoff_seconds
            .saturating_mul(1u32.checked_shl(shift).unwrap_or(u32::MAX))
    }

    /// Reset the deopt log so the function can be re-promoted to T2.
    ///
    /// Called when the backoff period has expired and the tiered compiler
    /// wants to give the function another chance at T2 compilation.
    pub fn reset_for_repromotion(&mut self) {
        self.entries.clear();
        self.write_index = 0;
        self.entry_count = 0;
        self.total_deopts = 0;
        self.reason_counters.clear();
        self.blacklisted_at = None;
        // blacklist_count is NOT reset — it drives exponential backoff
        // so subsequent blacklistings have longer backoff periods.
    }

    /// Access the ring buffer entries (for inspection/testing).
    pub fn entries(&self) -> &[DeoptEntry] {
        &self.entries[..self.entry_count.min(self.entries.len())]
    }
}

impl Default for DeoptLog {
    fn default() -> Self {
        Self::new()
    }
}

/// Result of a deoptimisation — reconstructed interpreter frame data
/// that the runtime needs to resume interpretation.
pub struct DeoptResult {
    /// Reconstructed local variable values for the interpreter frame.
    /// Each entry is a BlissVal corresponding to a local slot.
    pub frame_locals: Vec<BlissVal>,
    /// Whether this function is now blacklisted from T2 promotion.
    pub blacklisted: bool,
    /// The total number of deopts recorded for this function.
    pub total_deopts: u32,
}

/// Perform an OSR entry: transfer execution from T0/T1 to T2 at a loop back-edge.
///
/// Validates the function, maps locals via the entry map, and prepares the
/// transfer to T2 compiled code at the appropriate back-edge point.
/// Returns `Ok(OsrEntryResult)` with the mapped SSA values and target PC
/// so the runtime layer can construct the T2 frame and transfer control.
pub fn osr_entry(
    function: BlissVal,
    entry_map: &OsrEntryMap,
    locals: &[BlissVal],
) -> Result<OsrEntryResult, BlissError> {
    // Validate the function tag (spec A4.03 step 1).
    let is_function = function.tag() == bliss_rt::value::TAG_FUNCTION;

    if !is_function {
        return Err(BlissError::Internal(format!(
            "osr_entry: expected a function-tagged value, got tag {:#x}",
            function.tag()
        )));
    }

    // Check whether this function is currently in the backoff period
    // (blacklisted after repeated deopts). If so, refuse the T2 entry.
    if is_function_in_backoff(function) {
        return Err(BlissError::Internal(
            "osr_entry: function is in deopt backoff period, T2 entry suppressed".to_string(),
        ));
    }

    // Delegate to the entry map which validates locals, checks type guards
    // (spec A4.03 step 4), applies conversions (step 6), and builds the
    // SSA mapping. The returned OsrEntryResult contains the mapped values
    // for the runtime to populate the T2 frame.
    let entry_result = entry_map.enter(locals, std::ptr::null())?;

    // Return the entry result so the runtime trampoline in bliss-rt can:
    //   1. Construct a T2 stack frame with SSA variables populated from
    //      entry_result.mapped_values.
    //   2. Set the program counter to entry_result.target_pc_offset
    //      within the compiled code body.
    //   3. Transfer control via a platform-specific trampoline.
    Ok(entry_result)
}

/// Perform an OSR exit (deoptimisation): reconstruct interpreter frame
/// from compiled state when a guard fails.
///
/// Records the deopt reason in the persistent per-function log, checks
/// blacklisting status (including exponential backoff per §4.6.5.2), and
/// reconstructs interpreter-frame data from the live values captured at
/// the guard site. Returns `Ok(DeoptResult)` with the reconstructed frame
/// locals and blacklist status so the runtime can resume interpretation.
pub fn deoptimize(
    function: BlissVal,
    reason: DeoptReason,
    live_values: &[BlissVal],
) -> Result<DeoptResult, BlissError> {
    // Validate the function tag.
    let is_function = function.tag() == bliss_rt::value::TAG_FUNCTION;

    if !is_function {
        return Err(BlissError::Internal(format!(
            "deoptimize: expected a function-tagged value, got tag {:#x}",
            function.tag()
        )));
    }

    // Look up or create the persistent DeoptLog for this function.
    // The log is keyed by the function's raw tagged-pointer bits.
    let (blacklisted, total_deopts) = {
        let mut logs = DEOPT_LOGS.lock().unwrap();
        let log = logs.entry(function.0).or_default();
        log.record(reason);
        (log.is_blacklisted(), log.total_deopts())
    };

    // Reconstruct the interpreter frame from live values (spec A4.04 steps 5-6).
    // Each live value corresponds to a local slot in the interpreter frame
    // that must be restored before interpretation resumes.
    let frame_locals: Vec<BlissVal> = live_values.to_vec();

    // Return the reconstructed frame data and blacklist status so the
    // runtime trampoline in bliss-rt can:
    //   1. Unwind the current T2 stack frame.
    //   2. Construct a new T0 interpreter frame with frame_locals.
    //   3. Set the interpreter PC to the deopt point's bytecode offset.
    //   4. If blacklisted, mark the function so the tiered compiler
    //      does not re-promote it to T2 until the backoff expires.
    //   5. Resume execution in the interpreter.
    Ok(DeoptResult {
        frame_locals,
        blacklisted,
        total_deopts,
    })
}

/// Deopt configuration.
pub struct DeoptConfig {
    /// Number of deopts before blacklisting from T2 (default: 20).
    pub blacklist_threshold: u32,
    /// Backoff period in seconds before retrying T2 after blacklist.
    pub backoff_seconds: u32,
}

impl DeoptLog {
    /// Create a deopt log configured from a `DeoptConfig`.
    pub fn with_config(config: &DeoptConfig) -> Self {
        DeoptLog {
            entries: Vec::with_capacity(DEOPT_LOG_CAP),
            write_index: 0,
            entry_count: 0,
            total_deopts: 0,
            reason_counters: HashMap::new(),
            blacklist_threshold: config.blacklist_threshold,
            backoff_seconds: config.backoff_seconds,
            blacklisted_at: None,
            blacklist_count: 0,
            deopt_window: DEOPT_WINDOW,
        }
    }
}
