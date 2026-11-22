//! Optional, thread-local diagnostics for the VM benchmark.

use std::cell::Cell;
use std::time::Duration;

/// Whether VM diagnostic hooks were enabled at compile time.
pub const ENABLED: bool = option_env!("YIELD_VM_BENCH_DIAGNOSTICS").is_some();

#[derive(Clone, Copy, Debug, Default)]
pub struct Counters {
    /// Bytecode instructions dispatched, including instructions that throw.
    pub dispatches: u64,
    /// Frames constructed with fresh slot storage, including resume frames.
    /// A zero-slot frame counts as a construction, not a heap allocation.
    pub frame_allocations: u64,
    /// Total transient and persistent slots provisioned in those frames.
    pub frame_slots: u64,
    /// Own-property probes, hits and misses, including each own lookup in a
    /// prototype walk and specialized string, array and typed-array lookups.
    /// Does not count proxy dispatch, enumeration, or property table mutations.
    pub property_lookups: u64,
    /// Environments visited when searching for a binding owner, including misses.
    /// Does not count direct module loads or the final access in a found owner.
    pub binding_lookups: u64,
    /// Entries into the generic property-read prototype walk, once per entry,
    /// not per object visited. This is a baseline for that path, not an inline
    /// cache miss count (there is no inline cache).
    pub property_slow_paths: u64,
    /// Frame slot values retained in successful suspension captures.
    pub saved_values: u64,
    /// Successful await, yield (including delegation), and generator-start captures.
    pub suspensions: u64,
}

/// Wall-clock durations from a profiled VM construction.
#[derive(Clone, Copy, Debug)]
pub struct Startup {
    /// Validation, host linking, derived metadata, and VM setup, ending just
    /// before module instantiation.
    pub prepare: Duration,
    /// Bytecode version and build identity checks, including digest encoding.
    pub build_identity: Duration,
    /// Binary encoding and hashing of the build digest input.
    pub build_digest: Duration,
    /// Structural checks on modules, functions, and instructions.
    pub build_structure: Duration,
    /// Host linking, derived metadata, and VM setup after validation.
    pub link_and_setup: Duration,
    /// Module instantiation, evaluation, and draining initialization microtasks.
    pub initialize: Duration,
}

std::thread_local! {
    static COUNTERS: Cell<Counters> = const { Cell::new(Counters {
        dispatches: 0,
        frame_allocations: 0,
        frame_slots: 0,
        property_lookups: 0,
        binding_lookups: 0,
        property_slow_paths: 0,
        saved_values: 0,
        suspensions: 0,
    }) };
}

/// Clear the calling thread's diagnostics across all its VMs.
pub fn reset() {
    if ENABLED {
        COUNTERS.set(Counters::default());
    }
}

/// Read the calling thread's diagnostics, or zeros in an uninstrumented build.
pub fn read() -> Counters {
    if ENABLED {
        COUNTERS.get()
    } else {
        Counters::default()
    }
}

/// Update counters. Callers put the hook behind `if perf::ENABLED` so the
/// normal build does not reach this function even without inlining.
#[inline]
pub(crate) fn record(update: impl FnOnce(&mut Counters)) {
    COUNTERS.with(|cell| {
        let mut counters = cell.get();
        update(&mut counters);
        cell.set(counters);
    });
}

#[inline]
pub(crate) fn capture(values: usize) {
    record(|counters| {
        counters.suspensions += 1;
        counters.saved_values += values as u64;
    });
}
