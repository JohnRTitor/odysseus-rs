//! Login-path benchmarks. These produce the numbers in `docs/baseline/`.
//!
//! Run with `cargo bench -p odysseus-store --bench auth_hotpath`. Criterion
//! writes its per-benchmark JSON under `target/criterion/`; the committed copy
//! in `docs/baseline/` is the run this file was written against, with the
//! machine and the date recorded in `docs/baseline/README.md`.
//!
//! Every benchmark here measures something the application actually does on a
//! request. AGENTS.md §12.3 applies: a number nobody can reproduce is a comment.
//!
//! `main` returns `Result` rather than using `criterion_main!`. Benches are a
//! separate cargo target, so `clippy.toml`'s `allow-expect-in-tests` does not
//! cover them and the workspace's `expect_used = "deny"` does — the macro would
//! have forced `expect()` on every setup path. Propagating the error is both
//! cleaner and the only option that needs no `#[expect]`.

use std::{
    hint::black_box,
    path::Path,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use {
    criterion::{Criterion, Throughput},
    odysseus_store::{
        api_tokens::{Scope, TokenProfile, join_scopes, split_scopes},
        atomic::atomic_write_bytes,
        ratelimit::RateLimiter,
    },
    tempfile::TempDir,
};

/// The cost the Python app pays per login. `auth.json` stores cost-12 hashes, so
/// 12 is the number that matters and anything lower measures a different system.
const BCRYPT_COST: u32 = 12;

/// A cost-12 hash of a throwaway password, computed once outside the measured
/// region. Benchmarking hash *and* verify in one iteration would charge the
/// verify for two operations.
fn bcrypt_hash() -> Result<String, Box<dyn std::error::Error>> {
    Ok(bcrypt::hash("correct horse battery staple", BCRYPT_COST)?)
}

/// `atomic_write_bytes` fsyncs, so its cost is dominated by the filesystem rather
/// than by the bytes. That is exactly why it must be measured on a real temp dir:
/// a null filesystem would report a number nobody sees in production.
/// Nothing in here returns `Result`: the only fallible step is the write inside
/// the measurement loop, whose failures are counted and asserted below rather
/// than propagated. `b.iter` takes `FnMut() -> ()`, so a `?` could not cross the
/// closure anyway.
fn bench_atomic_write(c: &mut Criterion, dir: &TempDir) {
    let path = dir.path().join("auth.json");
    // Roughly the size of a small auth.json: a few users with bcrypt hashes and
    // privilege maps.
    let payload = vec![b'a'; 4 * 1024];
    let failures = AtomicUsize::new(0);
    // A failed write would make the whole measurement meaningless, so failures
    // are counted and asserted below rather than silently ignored. Recording them
    // through a closure keeps the timed region a single expression, which is both
    // faster and easier to read than an `if` in the loop body.
    let record = |ok: bool| {
        if !ok {
            failures.fetch_add(1, Ordering::Relaxed);
        }
    };

    let mut group = c.benchmark_group("atomic_write");
    group.throughput(Throughput::Elements(1));
    group.bench_function("write_4kib_fsync", |b| {
        b.iter(|| record(atomic_write_bytes(Path::new(&path), black_box(&payload)).is_ok()));
    });
    group.finish();

    assert_eq!(
        failures.load(Ordering::Relaxed),
        0,
        "every write in the measurement must succeed or the timing is meaningless"
    );
}

/// The rate limiter's `check` runs on every authenticated request, so its
/// per-call cost is multiplied by the request rate.
fn bench_ratelimit(c: &mut Criterion) {
    let mut group = c.benchmark_group("ratelimit");
    group.throughput(Throughput::Elements(1));

    // Hot key: the common case, a client inside its window.
    group.bench_function("check_hot_key", |b| {
        let limiter = RateLimiter::new(100, Duration::from_secs(60));
        b.iter(|| limiter.check(black_box("alice")));
    });

    // Cold key: the first request from a new client, which allocates a bucket.
    // This is the cost a credential-stuffing burst pays.
    group.bench_function("check_cold_key", |b| {
        let limiter = RateLimiter::new(100, Duration::from_secs(60));
        let mut n = 0u64;
        b.iter(|| {
            n += 1;
            limiter.check(black_box(&format!("client-{n}")));
        });
    });

    group.finish();
}

/// The token prefix cache: a bearer token resolves to scopes by its prefix, so
/// these are the parse/serialise costs behind every API authentication.
fn bench_token_prefix_cache(c: &mut Criterion) -> Result<(), Box<dyn std::error::Error>> {
    // Real scope names from `Scope::ALL` — a read plus the write it implies,
    // which is the shape a real token carries.
    let scopes = [
        Scope::parse("memory:read")?,
        Scope::parse("documents:read")?,
        Scope::parse("documents:write")?,
    ];
    let stored = join_scopes(&scopes);

    let mut group = c.benchmark_group("token_scopes");
    group.throughput(Throughput::Elements(1));

    group.bench_function("join_scopes", |b| {
        b.iter(|| join_scopes(black_box(&scopes)));
    });
    group.bench_function("split_scopes", |b| {
        b.iter(|| split_scopes(black_box(Some(stored.as_str()))));
    });
    group.bench_function("scope_parse", |b| {
        b.iter(|| Scope::parse(black_box("memory:read")));
    });

    group.bench_function("token_profile_parse", |b| {
        b.iter(|| TokenProfile::parse(black_box("codex_documents")));
    });

    group.finish();
    Ok(())
}

fn bench_bcrypt(c: &mut Criterion, hash: &str) {
    let mut group = c.benchmark_group("bcrypt_cost12");
    group.throughput(Throughput::Elements(1));

    // The login hot path, and the reason the dummy-hash work in the auth store
    // mattered. A wrong password costs the same as a right one — bcrypt is
    // constant-time by construction — so the two are measured together on
    // purpose: if they ever diverge, something has broken that property.
    group.bench_function("verify_correct_password", |b| {
        b.iter(|| bcrypt::verify(black_box("correct horse battery staple"), black_box(hash)));
    });
    group.bench_function("verify_wrong_password", |b| {
        b.iter(|| bcrypt::verify(black_box("wrong password"), black_box(hash)));
    });

    // Login, registration, and every settings write that re-hashes.
    group.bench_function("hash", |b| {
        b.iter(|| bcrypt::hash(black_box("correct horse battery staple"), BCRYPT_COST));
    });

    group.finish();
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let hash = bcrypt_hash()?;
    let dir: TempDir = tempfile::tempdir()?;
    let mut criterion = Criterion::default().configure_from_args();
    bench_bcrypt(&mut criterion, &hash);
    bench_atomic_write(&mut criterion, &dir);
    bench_ratelimit(&mut criterion);
    bench_token_prefix_cache(&mut criterion)?;
    criterion.final_summary();
    Ok(())
}
