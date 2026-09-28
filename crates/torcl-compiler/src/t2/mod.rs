//! T2 optimising compiler — block-based SSA (spec §4.3–§4.10).
//!
//! This module tree is the block-based-SSA T2 pipeline. It supersedes the older
//! top-level sea-of-nodes modules (`ir.rs`, `opt.rs`, `codegen.rs`, `osr.rs`),
//! which remain only until the T2 pipeline here lands and are then removed.
//!
//! ## Phase 0 — foundation (frozen contracts)
//!
//! The four modules below define the types and signatures every T2 parcel binds
//! to. They are lead-owned; parcels do not edit them (contract freeze).
//!
//! - [`ir`] — block-based SSA core: `Function`, `Block`, `Inst`, `Value`,
//!   `Opcode`, `IRType`, `ValueRepresentation`, dominance.
//! - [`frame_state`] — `FrameState` deopt metadata (§4.10 D4.15) + the
//!   preservation invariant it exists to support (R4.60).
//! - [`pass`] — the `Pass` trait, `Analyses` cache, and `PassManager`.
//! - [`mach`] — the lowering/regalloc interface: `MachInst`, `MachFunc`,
//!   `Location`, register classes, GC stack maps.
//!
//! ## Phase 1 — parcels (each in its own file; see the work-breakdown)
//!
//! P1 bytecode→SSA builder, P2 verifier, P3 inference, P4a–f optimisation
//! passes, P5 lowering, P6 register allocation, P7 deopt/metadata, P8 OSR entry.
//! Each lands as its own module here so sub-agents never share a file.

pub mod frame_state;
pub mod ir;
pub mod mach;
pub mod pass;
/// The shared per-safepoint liveness+representation producer that the deopt
/// export and the OSR import both read (bliss-ht4).
pub mod slot_map;
pub mod transfer_map;

// ── Wave 1 parcels (one module per sub-agent; disjoint files) ─────
pub mod build; // P1 — bytecode → SSA
pub mod infer; // P3 — type/range/representation inference
pub mod inlining; // call-site policy + compiler-known inline metadata
pub mod verify; // P2 — IR verifier (A4.07)

// ── Wave 2 parcels ────────────────────────────────────────────────
pub mod deopt; // P7 — FrameState → stack-map lowering (A4.14)
pub mod lower; // P5 — IR → MachFunc instruction selection
pub mod opt_dce; // P4c — deopt-aware DCE + rematerialisation
pub mod opt_guard; // P4d — guard elimination + hoisting
pub mod opt_gvn; // P4a — global value numbering / CSE
pub mod opt_licm; // P4b — loop-invariant code motion
pub mod regalloc;
pub mod speculate; // profile-guided single-type speculative lowering // P6 — regalloc2 adapter + stack maps

// ── Machine-code emission + pipeline driver ───────────────────────
pub mod drive; // bytecode → executable T2 code (the tiering primitive)
pub mod emit; // MachFunc → executable x86-64 bytes (spec §4.7)
pub mod emit_a64;
pub mod emit_ppc64le;
pub mod emit_s390x;
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
pub mod native_transfer;

// ── Wave 3 parcels ────────────────────────────────────────────────
pub mod opt_escape; // P4e — escape analysis + scalar replacement
pub mod opt_fold; // P4f — constant folding + strength reduction
pub mod osr_entry; // P8 — OSR entry region in the IR (A4.03, R4.66)
