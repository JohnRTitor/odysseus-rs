//! Allocation counts for the store's per-operation hot paths.
//!
//! AGENTS.md §12.2: "Report per-entry footprint **and** allocation count; a
//! counting `#[global_allocator]` wrapper is what makes the second number
//! obtainable." And §12.1 rule 6: "Every box is a separate allocation ... `no
//! extra allocation` is the goal, not `fewer bytes`."
//!
//! ## Why this file must run single-threaded
//!
//! The counting allocator is process-global. Under plain `cargo test`, other
//! tests in this binary allocate on other threads and their counts land in ours,
//! so the numbers become noise. Run it as:
//!
//! ```text
//! cargo test -p odysseus-store --test alloc_baseline -- --test-threads=1
//! ```
//!
//! Under `cargo nextest` each test is its own process, so the default CI gate
//! (`cargo nextest run --profile ci`) is already isolated and the numbers hold
//! there without the flag. That is why the suite's gate is nextest.
//!
//! The measured values are committed in `docs/baseline/`; the ceilings here are
//! what stops a regression shipping silently. Counters are compared, never
//! asserted for exact equality, because the allocator's internals (thread-local
//! caches, alignment padding) are not something this crate controls.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    hint::black_box,
    sync::atomic::{AtomicUsize, Ordering},
};

use odysseus_store::{
    api_tokens::{Scope, join_scopes, split_scopes},
    atomic::atomic_write_bytes,
};

// ---------------------------------------------------------------- allocator

static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static REALLOCS: AtomicUsize = AtomicUsize::new(0);

/// Wraps the system allocator and counts calls. Allocation *is* the thing being
/// measured, so this is the one place `unsafe` is unavoidable. Every method
/// forwards to `System` unchanged and adds no behaviour of its own; the SAFETY
/// note on the impl covers all of them.
///
/// The alternative — `dhat`, or `jemallocator`'s statistics — either needs a
/// nightly feature or replaces the allocator wholesale, and this has to be
/// measurable inside the ordinary `cargo nextest` gate.
struct Counting;

/// SAFETY: no pointer is read, written or reinterpreted here. `GlobalAlloc`
/// requires an allocator to honour the `Layout` it is given and to return memory
/// valid for it; delegating to `System` inherits that contract exactly, and the
/// `fetch_add` touches only an atomic, never the returned memory.
#[expect(
    unsafe_code,
    reason = "counting allocator: delegates to System, adds no behaviour"
)]
unsafe impl GlobalAlloc for Counting {
    #[expect(
        unsafe_code,
        reason = "unconditional delegation to the system allocator"
    )]
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: `layout` is forwarded unchanged.
        unsafe { System.alloc(layout) }
    }

    #[expect(
        unsafe_code,
        reason = "unconditional delegation to the system allocator"
    )]
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: `layout` is forwarded unchanged.
        unsafe { System.alloc_zeroed(layout) }
    }

    #[expect(
        unsafe_code,
        reason = "unconditional delegation to the system allocator"
    )]
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        REALLOCS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: `ptr`, `layout` and `new_size` are forwarded unchanged.
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    #[expect(
        unsafe_code,
        reason = "unconditional delegation to the system allocator"
    )]
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` and `layout` are forwarded unchanged.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Takes the counters, then resets them, so each measurement starts from zero.
/// `black_box` keeps the optimiser from eliding the region being measured.
fn measure<F: FnOnce()>(f: F) -> (usize, usize) {
    ALLOCS.store(0, Ordering::SeqCst);
    REALLOCS.store(0, Ordering::SeqCst);
    f();
    (
        ALLOCS.load(Ordering::SeqCst),
        REALLOCS.load(Ordering::SeqCst),
    )
}

// ---------------------------------------------------------------- tests

/// Ceilings, from the run committed in `docs/baseline/`. The headroom is
/// deliberate: the exact count depends on `serde_json` and `tempfile` internals,
/// which are not this crate's to control. What the ceiling is for is catching an
/// added copy of the payload, which would roughly double the figure.
const ATOMIC_WRITE_ALLOCATION_CEILING: usize = 40;
const SCOPE_ROUNDTRIP_ALLOCATION_CEILING: usize = 24;

/// Every measurement in one test function, deliberately.
///
/// The counting allocator is process-global, so a sibling `#[test]` allocating on
/// another thread lands its counts in this window. An earlier revision of this
/// file had three tests and was flaky under plain `cargo test` — passing with
/// `--test-threads=1` and failing without it. One test removes the hazard at the
/// source, which is stronger than asking the runner to serialise: `--test-threads=1`
/// does not stop a value dropped by a *finished* test from being freed inside the
/// next test's window.
///
/// Under `cargo nextest` each test is already its own process, so this is belt
/// and braces — but a benchmark harness that only works under one runner is not a
/// benchmark harness.
#[test]
fn allocation_baseline() -> Result<(), Box<dyn std::error::Error>> {
    // --- the counters themselves, or every number below is meaningless ---
    let (probe_allocs, probe_reallocs) = measure(|| {
        let v: Vec<u8> = Vec::with_capacity(1024);
        black_box(&v);
    });
    eprintln!("calibration: one 1 KiB Vec -> allocs={probe_allocs} reallocs={probe_reallocs}");
    assert_eq!(
        probe_allocs, 1,
        "a single Vec::with_capacity must count exactly one allocation"
    );
    assert_eq!(probe_reallocs, 0, "with_capacity does not reallocate");

    // --- atomic_write_bytes: the auth.json fsync path ---
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("auth.json");
    let payload = vec![b'a'; 4 * 1024];
    // Warm first: the temp-file machinery has one-off allocations that are not
    // part of the steady-state cost being measured.
    atomic_write_bytes(&path, black_box(&payload))?;

    let (allocs, reallocs) = measure(|| {
        // An unwrap rather than `?`: `measure` takes a `FnOnce() -> ()`, and the
        // write failing would have to abort the test anyway.
        atomic_write_bytes(&path, black_box(&payload)).expect("write");
    });
    let write_total = allocs + reallocs;
    eprintln!("atomic_write_bytes(4 KiB): allocs={allocs} reallocs={reallocs} total={write_total}");
    assert!(
        write_total <= ATOMIC_WRITE_ALLOCATION_CEILING,
        "atomic_write_bytes allocated {write_total} times (ceiling {ATOMIC_WRITE_ALLOCATION_CEILING}). \
         The payload must not be copied on the way to the temp file."
    );

    // --- the token prefix-cache scope round trip ---
    let scopes = [
        Scope::parse("memory:read").expect("memory:read"),
        Scope::parse("documents:read").expect("documents:read"),
    ];
    let _ = join_scopes(black_box(&scopes));

    let (allocs, reallocs) = measure(|| {
        let stored = join_scopes(black_box(&scopes));
        assert_eq!(split_scopes(Some(stored.as_str())), scopes);
    });
    let scope_total = allocs + reallocs;
    eprintln!("scope round trip: allocs={allocs} reallocs={reallocs} total={scope_total}");
    assert!(
        scope_total <= SCOPE_ROUNDTRIP_ALLOCATION_CEILING,
        "the scope round trip allocated {scope_total} times (ceiling \
         {SCOPE_ROUNDTRIP_ALLOCATION_CEILING}). AGENTS.md §12.1 rule 2 says one buffer \
         rather than N collections; scopes are small enough that an extra copy per call \
         is a real cost on the token path."
    );

    Ok(())
}
