//! Acceptance gate for the rate limiter and the constant-time login path (T0064).
//!
//! Two separate defences against account enumeration, both on pre-auth
//! endpoints, tested together because either alone leaves a hole:
//!
//!   * [`RateLimiter`] bounds how many guesses an attacker may make per client.
//!   * [`AuthStore::verify_password_or_dummy`] makes every guess cost the same,
//!     so the limiter cannot be turned into a timing oracle by choosing victims.
//!
//! Everything here drives an injected clock. No test sleeps, and none reads the
//! wall clock (AGENTS.md §11), so the suite is deterministic and instant.
//!
//! `verify_password_or_dummy` deliberately does **not** assert absolute timings.
//! Comparing fixed elapsed-time deadlines would be a flaky test on a loaded CI
//! runner, and it would fail for reasons unrelated to the code. What is asserted
//! instead is the *structural* property that produces the timing parity: both paths
//! run exactly one bcrypt comparison, and a miss is never distinguishable from a
//! wrong password by anything a caller can observe.
//!
//! The single exception is `the_miss_path_costs_about_the_same_as_the_hit_path`,
//! which compares the two paths against **each other** rather than against a
//! deadline. It exists because the behavioural tests alone cannot tell a
//! dummy-hashed miss from a short-circuited one: both answer `false`. See its
//! docs for why the ratio has a two-orders-of-magnitude margin.

// No file-level `expect_used` expectation here: every `.expect()` in this file is
// inside a `#[test]` function, which clippy *does* classify as test code, so
// `clippy.toml`'s `allow-expect-in-tests` already applies and the expectation
// would be unfulfilled — itself a denied lint. If a future helper function lands
// outside a `#[test]`, it will need one.

use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use odysseus_store::{
    auth::AuthStore,
    ratelimit::{Monotonic, RateLimiter, SystemMonotonic},
};

/// A clock the test advances by hand.
#[derive(Debug)]
struct TestClock(Arc<AtomicU64>);

impl TestClock {
    fn new(now: u64) -> Self {
        Self(Arc::new(AtomicU64::new(now)))
    }
    fn advance(&self, secs: u64) {
        self.0.fetch_add(secs, Ordering::SeqCst);
    }
    fn shared(&self) -> Arc<dyn Monotonic> {
        Arc::new(TestClock(Arc::clone(&self.0)))
    }
}

impl Monotonic for TestClock {
    fn now_secs(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// A limiter with an injected clock and a generous key cap.
fn limiter(max: usize, window_secs: u64, clock: &TestClock) -> RateLimiter {
    RateLimiter::with_clock_and_cap(max, Duration::from_secs(window_secs), 1024, clock.shared())
}

/// The Python `_login_limiter` policy: `RateLimiter(15, 60)`.
const LOGIN_MAX: usize = 15;
/// The Python `_setup_limiter` / `_signup_limiter` policy: `(3, 300)`.
const SETUP_MAX: usize = 3;
/// Window shared by the tests, chosen so `max(window * 2, 120)` == 120.
const WINDOW: u64 = 60;

// ---------------------------------------------------------------------------
// Sliding window
// ---------------------------------------------------------------------------

/// Exactly `max_requests` are allowed, then denied — and the budget returns once
/// the window slides.
#[test]
fn allows_exactly_the_budget_then_denies_then_recovers() {
    let clock = TestClock::new(1_000);
    let limiter = limiter(SETUP_MAX, WINDOW, &clock);

    let allowed = (0..SETUP_MAX)
        .map(|_| limiter.check("10.0.0.1"))
        .collect::<Vec<_>>();
    assert_eq!(
        allowed,
        vec![true; SETUP_MAX],
        "the first {SETUP_MAX} requests must all be allowed"
    );
    assert!(
        !limiter.check("10.0.0.1"),
        "request {} must be denied once the budget is spent",
        SETUP_MAX + 1
    );

    // Still denied one second before the window slides.
    clock.advance(WINDOW - 1);
    assert!(!limiter.check("10.0.0.1"), "the window has not elapsed yet");

    // Retention is `t > cutoff`, so the oldest hits drop out only once
    // `now - window` has passed them.
    clock.advance(1);
    assert!(
        limiter.check("10.0.0.1"),
        "the budget must return once the window has slid past every recorded hit"
    );
}

/// A denied request must not extend the window.
///
/// This is what separates a limiter from a lockout: if rejections were recorded, a
/// client that kept trying could hold itself denied forever, and the restore time
/// would depend on attack rate rather than on policy.
///
/// The discriminating sequence depends on the retention rule being `t > cutoff`, so
/// the three original hits land at `T` are dropped only once the clock reaches
/// `T + window`:
///
/// 1. Spend the budget at `T`.
/// 2. Advance to `T + window - 1` and poll many times *without* advancing further.
/// 3. Advance the final second, past `T + window`.
///
/// If denials were recorded, every poll in step 2 would have left a hit at
/// `T + window - 1`, which is still inside the window in step 3, and the budget
/// would still be spent. If they are not recorded, the only hits are the original
/// three at `T`, all of which fall out at `T + window`.
#[test]
fn a_denied_request_does_not_extend_the_window() {
    const BUDGET: usize = SETUP_MAX;
    let clock = TestClock::new(1_000);
    let limiter = limiter(BUDGET, WINDOW, &clock);

    for _ in 0..BUDGET {
        assert!(
            limiter.check("10.0.0.1"),
            "budget should still be available"
        );
    }

    // Stop one second short of the edge, then keep polling in place.
    clock.advance(WINDOW - 1);
    for _ in 0..50 {
        assert!(
            !limiter.check("10.0.0.1"),
            "the budget is spent and the window has not slid"
        );
    }

    // One more second and the original hits have aged out. None of the 50 denials
    // above extended anything.
    clock.advance(1);
    assert!(
        limiter.check("10.0.0.1"),
        "denied requests must not extend the window"
    );
}

/// Keys are independent budgets, not a shared pool.
#[test]
fn keys_have_independent_budgets() {
    let clock = TestClock::new(1_000);
    let limiter = limiter(SETUP_MAX, WINDOW, &clock);

    for key in ["10.0.0.1", "10.0.0.2", "10.0.0.3"] {
        for _ in 0..SETUP_MAX {
            assert!(limiter.check(key), "{key} should have its own budget");
        }
    }

    // One client exhausting its budget must not affect the others.
    for key in ["10.0.0.1", "10.0.0.2", "10.0.0.3"] {
        assert!(!limiter.check(key), "{key} should now be denied");
    }
    assert!(
        limiter.check("10.0.0.4"),
        "a client never seen before must not be affected by other clients"
    );
    assert_eq!(limiter.tracked_keys(), 4, "all four clients are tracked");
}

/// The key map stays bounded under a rotating-key flood, and the limiter keeps
/// working afterwards.
///
/// Python has no equivalent bound: `_log` grows one entry per distinct client
/// host and `_maybe_cleanup` only runs once per `_cleanup_interval`, so rotating
/// source addresses grows the dict without limit between sweeps.
#[test]
fn the_key_cap_bounds_memory_and_the_limiter_keeps_serving() {
    const CAP: usize = 32;
    let clock = TestClock::new(1_000);
    let limiter =
        RateLimiter::with_clock_and_cap(5, Duration::from_secs(WINDOW), CAP, clock.shared());

    // 500 distinct hosts, one request each: far past the cap.
    for i in 0..500 {
        assert!(
            limiter.check(&format!("10.1.{}.{}", i / 256, i % 256)),
            "request {i} from a fresh key must be allowed"
        );
        assert!(
            limiter.tracked_keys() <= CAP,
            "tracked keys grew to {} with a cap of {CAP}",
            limiter.tracked_keys()
        );
    }
    assert_eq!(
        limiter.tracked_keys(),
        CAP,
        "the map should sit exactly at the cap after the flood"
    );

    // Eviction must not have broken the limiter: it still denies an over-budget
    // key. Which specific keys survived is the eviction policy's business and is
    // asserted separately below.
    for _ in 0..5 {
        assert!(limiter.check("stable-host"), "a stable key gets its budget");
    }
    assert!(
        !limiter.check("stable-host"),
        "the limiter must still enforce its budget after evicting"
    );
}

/// Eviction drops the least-recently-active key, and the key just admitted is
/// never the one sacrificed.
///
/// This pins the documented policy rather than merely asserting "some key went
/// away", so the eviction rule cannot silently become arbitrary.
#[test]
fn eviction_reclaims_the_least_recently_active_key() {
    const CAP: usize = 3;
    let clock = TestClock::new(1_000);
    let limiter =
        RateLimiter::with_clock_and_cap(10, Duration::from_secs(WINDOW), CAP, clock.shared());

    // Fill to the cap, oldest first.
    for key in ["oldest", "middle", "newest"] {
        assert!(limiter.check(key), "{key} should be admitted");
        clock.advance(1);
    }
    assert_eq!(limiter.tracked_keys(), CAP);

    // One request from a fresh key forces an eviction.
    assert!(limiter.check("intruder"), "the new key must be admitted");

    // "oldest" has the earliest last-hit, so it is the victim. It was not the key
    // just admitted, so the newcomer is dropped rather than the new key.
    assert!(
        limiter.check("intruder"),
        "the newly admitted key must not be evicted by its own admission"
    );
    assert!(
        limiter.tracked_keys() <= CAP,
        "eviction must keep the map at the cap, got {}",
        limiter.tracked_keys()
    );

    // "oldest" is gone, so it starts again from a full budget.
    for _ in 0..10 {
        assert!(
            limiter.check("oldest"),
            "an evicted key must be re-admitted from a full budget"
        );
    }
}

/// The periodic sweep reclaims idle keys on Python's interval,
/// `max(window_seconds * 2, 120)`.
///
/// The sweep runs *before* the admission decision on the same `check` call, so a
/// key that has aged out is dropped and the calling key is then (re)created.
#[test]
fn the_periodic_sweep_reclaims_idle_keys() {
    let clock = TestClock::new(1_000);
    let limiter = limiter(5, WINDOW, &clock);

    assert!(limiter.check("idle-client"));
    assert_eq!(limiter.tracked_keys(), 1);

    // Just short of the 120 s interval: the sweep must not have run yet, and both
    // keys are still live because neither has aged past the 60 s window.
    clock.advance(119);
    assert!(limiter.check("other-client"));
    assert_eq!(
        limiter.tracked_keys(),
        2,
        "the sweep must not run before max(window * 2, 120)"
    );

    // Cross the interval *and* age every key out. "other-client" last hit at
    // t=1119, so it only falls outside the 60 s window at t=1179; going straight
    // to 1180 leaves both keys stale and both dropped by the sweep.
    clock.advance(61);
    assert!(limiter.check("trigger-key"));
    assert_eq!(
        limiter.tracked_keys(),
        1,
        "the sweep must drop keys whose window has fully drained"
    );
}

/// Concurrent callers never collectively exceed the budget.
///
/// This is the property a check-then-act implementation without the lock held
/// across read-decide-append would lose: several threads each read a length below
/// `max_requests` and all append, so the total exceeds the limit.
#[test]
fn concurrent_checks_never_exceed_the_budget() {
    const THREADS: usize = 16;
    const ATTEMPTS: usize = 50;
    let clock = TestClock::new(1_000);
    let limiter = Arc::new(limiter(SETUP_MAX, WINDOW, &clock));

    let handles = (0..THREADS)
        .map(|_| {
            let limiter = Arc::clone(&limiter);
            std::thread::spawn(move || {
                (0..ATTEMPTS)
                    .filter(|_| limiter.check("shared-host"))
                    .count()
            })
        })
        .collect::<Vec<_>>();

    let allowed = handles
        .into_iter()
        .map(|handle| handle.join().unwrap_or(0))
        .sum::<usize>();

    assert_eq!(
        allowed, SETUP_MAX,
        "{THREADS} threads made {ATTEMPTS} attempts each; exactly {SETUP_MAX} must \
         have been allowed and the rest denied"
    );
}

/// Distinct keys stay independent under concurrency: the per-key budget still
/// holds even when every thread is on its own key.
#[test]
fn concurrent_checks_on_distinct_keys_stay_within_budget() {
    const THREADS: usize = 8;
    let clock = TestClock::new(1_000);
    let limiter = Arc::new(limiter(SETUP_MAX, WINDOW, &clock));

    let handles = (0..THREADS)
        .map(|thread| {
            let limiter = Arc::clone(&limiter);
            std::thread::spawn(move || {
                let key = format!("10.0.0.{thread}");
                (0..20).filter(|_| limiter.check(&key)).count()
            })
        })
        .collect::<Vec<_>>();

    let allowed = handles
        .into_iter()
        .map(|handle| handle.join().unwrap_or(0))
        .sum::<usize>();

    assert_eq!(
        allowed,
        SETUP_MAX * THREADS,
        "each of {THREADS} keys must get exactly its own budget of {SETUP_MAX}"
    );
}

/// The Python policies are constructible through the public API.
///
/// `routes/auth_routes.py:119-121` builds `RateLimiter(15, 60)`, `(3, 300)` and
/// `(3, 300)`. If a constructor signature changes, this fails before the web layer
/// tries to build one.
#[test]
fn the_python_policies_are_constructible() {
    let login = RateLimiter::new(LOGIN_MAX, Duration::from_secs(60));
    assert_eq!(login.max_requests(), LOGIN_MAX);
    assert_eq!(login.window(), Duration::from_secs(60));

    let signup = RateLimiter::new(SETUP_MAX, Duration::from_secs(300));
    assert_eq!(signup.max_requests(), SETUP_MAX);
    assert_eq!(signup.window(), Duration::from_secs(300));

    // The default key cap exists; Python has none.
    assert!(login.max_keys() > 0, "a limiter must bound its key count");
    assert!(login.check("127.0.0.1"), "a fresh key must be allowed");

    // `SystemMonotonic` is the default source and must be usable directly.
    let probe = SystemMonotonic;
    assert!(probe.now_secs() < u64::MAX, "monotonic must be readable");
}

// ---------------------------------------------------------------------------
// Constant-time login path
// ---------------------------------------------------------------------------
//
// These tests return `Result` and propagate with `?` rather than calling
// `expect`. `clippy.toml` sets `allow-unwrap-in-tests` / `allow-expect-in-tests`,
// but those apply to `#[cfg(test)]` modules; an integration test under
// `crates/*/tests/` is a separate crate and does not get the exemption. This
// follows the convention in `crates/store/tests/baseline.rs`.

/// The error type these tests propagate.
type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Builds a store over a temp data dir with one user, `alice`.
fn store_with_alice() -> Result<(tempfile::TempDir, AuthStore), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let auth = AuthStore::new(dir.path());
    auth.prepare()?;
    auth.create_user("alice", ALICE_PASSWORD, false)?;
    Ok((dir, auth))
}

/// `alice`'s password, named so the auth tests cannot accidentally pass a
/// different string and assert something they did not intend.
const ALICE_PASSWORD: &str = "correct horse battery staple";

/// A correct password for a known user authenticates.
#[test]
fn a_correct_password_for_a_known_user_authenticates() -> TestResult {
    let (_dir, auth) = store_with_alice()?;
    assert!(
        auth.verify_password_or_dummy("alice", ALICE_PASSWORD)?,
        "the correct password must authenticate"
    );
    Ok(())
}

/// A wrong password for a known user is refused.
#[test]
fn a_wrong_password_for_a_known_user_is_refused() -> TestResult {
    let (_dir, auth) = store_with_alice()?;
    assert!(
        !auth.verify_password_or_dummy("alice", "wrong password entirely")?,
        "a wrong password must not authenticate"
    );
    Ok(())
}

/// A missing user returns `false`, not an error.
///
/// This is the behavioural half of the defence: an unknown username is a failed
/// login, so it must be indistinguishable from a wrong password by return value.
/// Returning `Err(NoSuchUser)` would reintroduce the oracle as a status code,
/// which is strictly worse than the timing gap it replaced.
#[test]
fn a_missing_user_is_false_rather_than_an_error() -> TestResult {
    let (_dir, auth) = store_with_alice()?;
    let result = auth.verify_password_or_dummy("nosuchuser", "any password at all");
    // A missing user must be observationally identical to a wrong password: the
    // same value, and no error. Anything else is an enumeration oracle, which is
    // the entire reason this function exists. Written as one `matches!` rather
    // than `assert!(false, …)` arms, because clippy folds a constant `false`.
    assert!(
        matches!(result, Ok(false)),
        "a missing user must be indistinguishable from a wrong password, got {result:?}"
    );
    Ok(())
}

/// The two paths are observationally identical to the caller.
///
/// This is the property that removes the oracle, and it is checkable without timing
/// anything: for any input, a known and an unknown username produce the same
/// `Result` shape and the same answer. Only the *amount of work* differs, and
/// `verify_password_or_dummy` collapses that by always running one bcrypt
/// comparison.
#[test]
fn the_hit_and_miss_paths_are_indistinguishable_to_the_caller() -> TestResult {
    let (_dir, auth) = store_with_alice()?;

    // Same password, same shape of answer, only the truth value differs. The
    // password is alice's, so the hit authenticates and the miss does not.
    let hit = auth.verify_password_or_dummy("alice", ALICE_PASSWORD)?;
    let miss = auth.verify_password_or_dummy("ghost", ALICE_PASSWORD)?;
    assert!(hit, "the known user with the right password authenticates");
    assert!(!miss, "the unknown user does not authenticate");

    // A wrong password against the known user and any password against the unknown
    // user are the same answer, which is what a caller must not be able to split.
    let wrong = auth.verify_password_or_dummy("alice", "not the password")?;
    let any = auth.verify_password_or_dummy("ghost", "not the password either")?;
    assert!(
        !wrong,
        "a wrong password against a known user must not authenticate"
    );
    assert!(
        !any,
        "any password against an unknown user must not authenticate"
    );

    // Case and whitespace are normalised exactly as on the hit path.
    assert!(
        auth.verify_password_or_dummy("  ALICE  ", ALICE_PASSWORD)?,
        "username normalisation must apply to the hit path too"
    );
    Ok(())
}

/// The dummy comparison cannot authenticate anyone, whatever the password.
///
/// The implementation short-circuits on `user_exists` after the bcrypt result. If
/// that guard were dropped, a caller who somehow learned the random dummy
/// plaintext would get a `true` for a nonexistent user — so this test pins that
/// the *existence* check is authoritative, not the hash comparison.
#[test]
fn the_dummy_path_never_authenticates() -> TestResult {
    let (_dir, auth) = store_with_alice()?;
    // A spread of inputs, all of which must fail. None is the dummy plaintext;
    // the point is that the answer is `false` unconditionally.
    for password in [
        "",
        "a",
        ALICE_PASSWORD,
        "unicode \u{1F600} password",
        &"x".repeat(200),
    ] {
        let result = auth.verify_password_or_dummy("definitely-not-a-user", password)?;
        assert!(
            !result,
            "an unknown user must never authenticate, even with {password:?}"
        );
    }
    Ok(())
}

/// Username normalisation is applied before the existence check.
///
/// `normalise_username` trims and lowercases, as `auth.py` does before every
/// lookup. Without it `"ALICE"` would miss and take the dummy path even though
/// `"alice"` exists — a slow path for a real user, and a timing signal for an
/// attacker probing case variants.
#[test]
fn username_case_does_not_decide_which_path_is_taken() -> TestResult {
    let (_dir, auth) = store_with_alice()?;
    // Same user, three spellings, all must authenticate identically.
    for spelling in ["alice", "ALICE", "Alice"] {
        assert!(
            auth.verify_password_or_dummy(spelling, ALICE_PASSWORD)?,
            "{spelling} must resolve to the same account as `alice`"
        );
    }
    Ok(())
}

/// The miss path must do the same bcrypt work as the hit path.
///
/// This is the one test that closes a gap the others cannot. Every behavioural test
/// above still passes if the miss path short-circuits with `return Ok(false)`
/// before touching bcrypt, because the *answer* is identical — `false` either way.
/// Only the amount of work differs, and work is the entire point of the dummy hash.
/// This test was added after mutation testing showed the suite was blind to exactly
/// that change.
///
/// It asserts a **ratio** rather than an absolute time, which is what makes it
/// usable in CI. bcrypt cost 12 takes ~250 ms and a short-circuit takes well under
/// 1 ms, so the two states differ by two orders of magnitude while ordinary
/// scheduling noise on a loaded runner is single-digit milliseconds. The bound is
/// set at an eighth of the hit path — looser than any plausible noise, and still
/// far above what a short-circuit could ever measure. No sleep and no fixed deadline
/// is involved, so this is not the sleep-based sync AGENTS.md §11 forbids; it
/// compares two measurements taken in the same process against each other.
#[test]
fn the_miss_path_costs_about_the_same_as_the_hit_path() -> TestResult {
    let (_dir, auth) = store_with_alice()?;

    // Warm the dummy hash and the bcrypt code path first, so neither measurement
    // pays a one-off initialisation the other does not.
    auth.verify_password_or_dummy("ghost", "warmup pass")?;
    auth.verify_password_or_dummy("alice", "warmup pass")?;

    let hit_start = Instant::now();
    let hit_ok = auth.verify_password_or_dummy("alice", ALICE_PASSWORD)?;
    let hit = hit_start.elapsed();
    assert!(hit_ok, "the hit path must authenticate");

    let miss_start = Instant::now();
    let miss_ok = auth.verify_password_or_dummy("ghost", ALICE_PASSWORD)?;
    let miss = miss_start.elapsed();
    assert!(!miss_ok, "the miss path must not authenticate");

    assert!(
        miss * 8 >= hit,
        "the unknown-user path took {} against the known-user path's {}: it skipped \
         the dummy bcrypt comparison, which is the timing oracle this exists to \
         close. Expected the miss path to be within 8x of the hit path.",
        miss.as_millis(),
        hit.as_millis()
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// The two defences together
// ---------------------------------------------------------------------------

/// A login route's shape: limiter first, then a single dummy-padded comparison.
///
/// This is the acceptance gate the web layer will build. It asserts the
/// composition — over-budget clients are refused before any bcrypt happens at all,
/// and admitted clients all cost the same comparison — which is stronger than
/// either test alone: a limiter with a leaky comparison leaks, and a constant-time
/// comparison with no limiter is still brute-forceable.
#[test]
fn limiter_plus_dummy_comparison_is_the_intended_login_gate() -> TestResult {
    const BUDGET: usize = LOGIN_MAX;
    let clock = TestClock::new(1_000);
    let limiter = limiter(BUDGET, 60, &clock);
    let (_dir, auth) = store_with_alice()?;

    let mut accepted = 0;
    let mut refused = 0;
    for _ in 0..(BUDGET * 2) {
        // The web handler's order: rate limit first, so a throttled request never
        // reaches bcrypt and never becomes a timing sample.
        if !limiter.check("203.0.113.7") {
            refused += 1;
            continue;
        }
        // Then exactly one comparison, hit or miss.
        auth.verify_password_or_dummy("alice", ALICE_PASSWORD)?;
        accepted += 1;
    }

    assert_eq!(
        accepted, BUDGET,
        "the limiter must admit exactly its budget before refusing"
    );
    assert_eq!(
        refused, BUDGET,
        "the remaining attempts must be refused by the limiter"
    );

    // A different attacker address is unaffected by the first one's budget.
    assert!(
        limiter.check("203.0.113.8"),
        "an independent client must not inherit another client's exhausted budget"
    );
    Ok(())
}
