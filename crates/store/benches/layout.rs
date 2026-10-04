//! Per-entry footprint for the types the store keeps alive in bulk.
//!
//! AGENTS.md §12.1 rule 7: "`assert!(size_of::<T>() <= N)` in a test is worth
//! more than a comment claiming a struct is small." Those ceilings are asserted
//! in `crates/store/tests/layout_ceilings.rs` so a regression fails the build;
//! this bench measures the same values so the committed baseline has real numbers
//! behind it rather than the assertions alone.
//!
//! §12.1's five rules are about *shape*, not just bytes: a `String` in a
//! per-session record costs 24 bytes of pointer/length/capacity that say nothing
//! once the value is stored, which is why the ceilings there are tight enough
//! that swapping a `String` for a `Box<str>` shows up in the diff.
//!
//! `size_of` is a compile-time constant, so there is nothing to time. The
//! benchmark exists to emit the values in a form the baseline file quotes; the
//! gate is `layout_ceilings.rs`, not this.

use std::{hint::black_box, mem::size_of};

use {
    criterion::{Criterion, Throughput},
    odysseus_store::auth::{Privileges, User},
};

fn main() {
    // Nothing here can fail, so `main` returns nothing — a `Result` would be
    // `clippy::unnecessary_wraps`.
    let mut criterion = Criterion::default().configure_from_args();
    let mut group = criterion.benchmark_group("layout_size_of");
    group.throughput(Throughput::Elements(1));
    // The closure returns the tuple rather than discarding it, which is what
    // keeps `black_box` from being optimised away.
    group.bench_function("size_of", |b| {
        b.iter(|| {
            black_box((
                size_of::<User>(),
                size_of::<Privileges>(),
                size_of::<String>(),
                size_of::<Option<String>>(),
                size_of::<Box<str>>(),
            ));
        });
    });
    group.finish();
    criterion.final_summary();
}
