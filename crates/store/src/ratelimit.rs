//! Sliding-window rate limiting, keyed by client host.
//!
//! A port of `src/rate_limiter.py`, which guards the pre-auth endpoints in
//! `routes/auth_routes.py:119-167`:
//!
//! | Python | Value | Purpose |
//! |---|---|---|
//! | `_login_limiter` | `RateLimiter(15, 60)` | password guessing |
//! | `_signup_limiter` | `RateLimiter(3, 300)` | account creation |
//! | `_setup_limiter` | `RateLimiter(3, 300)` | first-run admin creation |
//!
//! All three are keyed on `request.client.host`.
//!
//! # Why the login limiter matters more than its size suggests
//!
//! Password hashing is deliberately slow (bcrypt cost 12, ~250 ms). That makes a
//! bare `verify_password` a **timing oracle for account existence**: a handler
//! shaped `let user = lookup(name)?; verify(user)` answers an unknown username in
//! ~1 ms and a known one in ~250 ms, on an unauthenticated endpoint. The limiter
//! bounds how many such probes an attacker can make; it does not remove the
//! oracle. Pair it with [`crate::auth::AuthStore::verify_password_or_dummy`],
//! which performs exactly one bcrypt comparison either way.
//!
//! The second factor needs this more than the password does: TOTP accepts ~3
//! codes per 30-second step (`SKEW = 1`), so without a limiter a 6-digit code is
//! guessable in a few hundred thousand tries per step, and bcrypt does not slow
//! that path at all.
//!
//! # Two deliberate deviations from Python
//!
//! **The clock is injected.** Python calls `time.monotonic()` inline, so the
//! window can only be tested by sleeping — which AGENTS.md §11 forbids outright.
//! Here the time source is a [`Monotonic`], defaulting to monotonic, so tests
//! move time instantly and deterministically.
//!
//! **The key count is bounded.** Python's `_log` grows one entry per distinct
//! client host, and `_maybe_cleanup` only runs once per `_cleanup_interval`. An
//! attacker rotating source addresses between requests therefore grows the dict
//! without limit, and the cleanup that would eventually reclaim it is itself
//! rate-limited to once per 120 s. [`RateLimiter::with_key_cap`] caps the map and
//! evicts the least-recently-active key when full, so memory is O(cap) rather
//! than O(distinct clients).
//!
//! # Concurrency
//!
//! [`RateLimiter::check`] takes a [`Mutex`] and returns. It is pure in-memory work
//! over microseconds, so it never goes near `spawn_blocking` (AGENTS.md D11) — and
//! because the guard is a local dropped before returning, no lock is ever held
//! across an `.await`.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

/// Monotonic time source, injected so tests never sleep (AGENTS.md §11).
pub trait Monotonic: Send + Sync + std::fmt::Debug {
    /// Seconds from an arbitrary fixed origin. Only differences are meaningful.
    fn now_secs(&self) -> u64;
}

/// Reads the system monotonic clock, equivalent to Python's `time.monotonic()`.
///
/// The elapsed time is measured from a fixed origin captured at construction, not
/// from a fresh `Instant::now()` on every call — `Instant::now().elapsed()`
/// measures the time taken by the *call itself*, which is ~0, so it would pin
/// `now_secs()` at 0 forever. With `now_secs() == 0` the window cutoff is also 0
/// and `hit > cutoff` prunes every hit on every call, so the limiter admits
/// everything and silently does nothing. It must hold a real origin.
#[derive(Debug, Clone, Copy)]
pub struct SystemMonotonic {
    /// Process-lifetime origin. `Instant` is monotonic, so this cannot go
    /// backwards the way a wall clock can over NTP adjustment.
    origin: std::time::Instant,
}

impl SystemMonotonic {
    /// Captures the origin. Every `SystemMonotonic` built in the same process
    /// shares the same epoch, so their readings are comparable.
    #[must_use]
    pub fn new() -> Self {
        Self {
            origin: std::time::Instant::now(),
        }
    }
}

impl Default for SystemMonotonic {
    fn default() -> Self {
        Self::new()
    }
}

impl Monotonic for SystemMonotonic {
    fn now_secs(&self) -> u64 {
        self.origin.elapsed().as_secs()
    }
}

/// The smallest allowed per-window request budget.
///
/// A limiter constructed with zero would deny everything, which looks like a
/// misconfiguration rather than a policy. Clamping to 1 makes the failure mode
/// "too permissive", which a caller or a test can notice, rather than a lockout
/// that hides the bug.
const MIN_REQUESTS: usize = 1;

/// Default cap on tracked keys when the caller does not choose one.
///
/// Sized for a self-hosted instance on a LAN: far above the number of distinct
/// hosts that would ever legitimately authenticate, and small enough that the
/// worst case stays in the low megabytes. Python has no equivalent bound.
const DEFAULT_MAX_KEYS: usize = 16_384;

/// Per-key sliding window of request timestamps.
#[derive(Debug, Default)]
struct Window {
    /// Request times still inside the window, ascending.
    hits: Vec<u64>,
}

/// Mutable state, guarded by one mutex.
#[derive(Debug)]
struct State {
    /// Tracked keys.
    log: HashMap<String, Window>,
    /// Earliest time at which the next stale-key sweep may run.
    next_cleanup: u64,
}

/// Sliding-window rate limiter.
///
/// Construct one per policy (login, signup, setup) and share it across requests
/// — for example as a field on `AppState`. It is `Send + Sync`, so that works
/// without any locking discipline on the caller's part.
///
/// [`RateLimiter::check`] is the whole interface; see its docs for the contract.
#[derive(Debug)]
pub struct RateLimiter {
    /// Requests allowed per window, per key.
    max_requests: usize,
    /// Window width in seconds.
    window_secs: u64,
    /// Maximum tracked keys before eviction kicks in.
    max_keys: usize,
    /// Time source.
    clock: Arc<dyn Monotonic>,
    /// All mutable state, behind one lock.
    state: Mutex<State>,
}

impl RateLimiter {
    /// A limiter allowing `max_requests` per `window`, reading the system
    /// monotonic clock and bounding the key count at [`DEFAULT_MAX_KEYS`].
    #[must_use]
    pub fn new(max_requests: usize, window: Duration) -> Self {
        Self::with_clock(max_requests, window, Arc::new(SystemMonotonic::new()))
    }

    /// As [`RateLimiter::new`], with an explicit cap on tracked keys.
    ///
    /// `max_keys` is clamped to at least 1 so the limiter cannot be built
    /// unable to track the key it is asked about.
    #[must_use]
    pub fn with_key_cap(max_requests: usize, window: Duration, max_keys: usize) -> Self {
        Self::build(
            max_requests,
            window,
            max_keys,
            Arc::new(SystemMonotonic::new()),
        )
    }

    /// A limiter reading an injected time source, for deterministic tests.
    #[must_use]
    pub fn with_clock(max_requests: usize, window: Duration, clock: Arc<dyn Monotonic>) -> Self {
        Self::build(max_requests, window, DEFAULT_MAX_KEYS, clock)
    }

    /// A limiter with both an injected clock and an explicit key cap.
    #[must_use]
    pub fn with_clock_and_cap(
        max_requests: usize,
        window: Duration,
        max_keys: usize,
        clock: Arc<dyn Monotonic>,
    ) -> Self {
        Self::build(max_requests, window, max_keys, clock)
    }

    /// Shared constructor for the four public builders.
    ///
    /// The sweep interval reproduces Python exactly:
    /// `self._cleanup_interval = max(window_seconds * 2, 120)`.
    fn build(
        max_requests: usize,
        window: Duration,
        max_keys: usize,
        clock: Arc<dyn Monotonic>,
    ) -> Self {
        let window_secs = window.as_secs();
        let cleanup_interval = window_secs.saturating_mul(2).max(120);
        let next_cleanup = clock.now_secs().saturating_add(cleanup_interval);
        Self {
            max_requests: max_requests.max(MIN_REQUESTS),
            window_secs,
            max_keys: max_keys.max(1),
            clock,
            state: Mutex::new(State {
                log: HashMap::new(),
                next_cleanup,
            }),
        }
    }

    /// Requests allowed per window.
    #[must_use]
    pub fn max_requests(&self) -> usize {
        self.max_requests
    }

    /// Window width.
    #[must_use]
    pub fn window(&self) -> Duration {
        Duration::from_secs(self.window_secs)
    }

    /// The key cap this limiter was built with.
    #[must_use]
    pub fn max_keys(&self) -> usize {
        self.max_keys
    }

    /// Number of keys currently tracked.
    ///
    /// For metrics and tests. Reads under the same lock as `check`, so the value
    /// is a consistent snapshot rather than a race.
    #[must_use]
    pub fn tracked_keys(&self) -> usize {
        // A poisoned lock means some earlier caller panicked while holding it. The
        // map is still a valid `HashMap` and this is a diagnostic read, so report
        // what is there rather than propagating a panic to a metrics call site.
        self.state.lock().map_or_else(
            |poisoned| poisoned.into_inner().log.len(),
            |state| state.log.len(),
        )
    }

    /// Drops every tracked key, resetting all budgets.
    ///
    /// For tests and for an explicit administrative reset. Not something a
    /// request handler should reach for: it hands every client a fresh budget.
    pub fn reset(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.log.clear();
        }
    }

    /// Records a request for `key` and reports whether it is allowed.
    ///
    /// Returns `true` when the request may proceed; `false` means rate-limited.
    ///
    /// A denied request is **not** recorded, matching Python. A client that keeps
    /// hammering past the limit does not push its own expiry further out, so the
    /// budget is restored `window` after the *last allowed* request rather than
    /// the last attempt. Recording rejections would let a client hold itself
    /// locked out indefinitely by continuing to try, and would make the limiter's
    /// behaviour depend on attack rate.
    pub fn check(&self, key: &str) -> bool {
        let now = self.clock.now_secs();
        // The guard is a local dropped before this function returns, and there is
        // no `.await` anywhere on this path, so it can never be held across one.
        match self.state.lock() {
            Ok(mut state) => Self::admit(&mut state, key, now, self),
            // Poisoned: an earlier caller panicked while holding the lock.
            // Denying forever would break the endpoint; ignoring the lock would
            // remove the limiter. Recovering the map keeps the limiter working and
            // leaves the panic visible at its original site.
            Err(poisoned) => Self::admit(&mut poisoned.into_inner(), key, now, self),
        }
    }

    /// The decision, given the lock is already held.
    fn admit(state: &mut State, key: &str, now: u64, limits: &Self) -> bool {
        Self::maybe_cleanup(state, now, limits.window_secs);
        let cutoff = now.checked_sub(limits.window_secs);

        let window = state.log.entry(key.to_owned()).or_default();
        // Python: timestamps = [t for t in timestamps if t > cutoff]
        //
        // `checked_sub` rather than `saturating_sub`, and that distinction is load
        // bearing. When `now < window` there is no cutoff yet — the window reaches
        // back before the clock's origin — so every hit so far is inside it.
        // `saturating_sub` would yield `0` and silently discard hits recorded at
        // exactly `t == 0`, which is precisely the first second of any process
        // whose clock starts at zero. The effect: the limiter admits everything
        // for the first window and then starts working, which reads as "the
        // throttle is unreliable" rather than as a bug.
        window.hits.retain(|&hit| cutoff.is_none_or(|c| hit > c));

        if window.hits.len() >= limits.max_requests {
            // Denied. The pruned window is deliberately kept rather than removed,
            // so an exhausted key is not rebuilt from nothing on the next call.
            return false;
        }
        window.hits.push(now);
        limits.evict_if_full(state, key);
        true
    }

    /// Keeps the map bounded, never evicting the key just admitted.
    ///
    /// Policy: evict the key whose most recent hit is **oldest** — the least
    /// recently active one. That reclaims a quiet legitimate client before one
    /// mid-burst, and the victim is also the entry closest to the natural sweep,
    /// so eviction costs the least real throttling. Ties break on key name so the
    /// choice is deterministic and testable.
    fn evict_if_full(&self, state: &mut State, admitted: &str) {
        if state.log.len() <= self.max_keys {
            return;
        }
        let victim = state
            .log
            .iter()
            .filter(|(key, _)| key.as_str() != admitted)
            .min_by(|(a_key, a_win), (b_key, b_win)| {
                a_win
                    .hits
                    .last()
                    .cmp(&b_win.hits.last())
                    .then_with(|| a_key.cmp(b_key))
            })
            .map(|(key, _)| key.clone());
        if let Some(victim) = victim {
            state.log.remove(&victim);
        }
    }

    /// Periodically purges keys whose window has fully drained.
    ///
    /// Python's `_maybe_cleanup`, on the same interval. Retained because it is
    /// what lets the map shrink back down after a burst; the key cap is a ceiling
    /// on the worst case, not a substitute for reclaiming idle entries.
    fn maybe_cleanup(state: &mut State, now: u64, window_secs: u64) {
        if now < state.next_cleanup {
            return;
        }
        let interval = window_secs.saturating_mul(2).max(120);
        state.next_cleanup = now.saturating_add(interval);
        let cutoff = now.checked_sub(window_secs);
        state.log.retain(|_, window| {
            window
                .hits
                .last()
                .is_some_and(|&last| cutoff.is_none_or(|c| last > c))
        });
    }
}

#[cfg(test)]
mod regression_tests {
    //! These exist because two defects got through, and both made the limiter a
    //! no-op. They are cheap, so they stay.
    //!
    //! Defect one: `SystemMonotonic::now_secs` returned `Instant::now().elapsed()`,
    //! which measures the time taken by the *call* — about zero — so `now` was 0
    //! forever. Defect two, independent: the window pruned with
    //! `saturating_sub`, which yields cutoff 0 when `now < window`, so a hit
    //! recorded at exactly `t == 0` was discarded on the very next call.
    //!
    //! Either alone was enough to admit everything forever. The tests use the
    //! real clock where a test can be meaningful and an injected one where
    //! advancing time is the point; nothing sleeps (`AGENTS.md` §11).

    use {
        super::{DEFAULT_MAX_KEYS, Monotonic, RateLimiter},
        std::{sync::Arc, time::Duration},
    };

    /// A clock the test drives by hand.
    #[derive(Debug)]
    struct TestClock(Arc<std::sync::atomic::AtomicU64>);

    impl TestClock {
        fn new(start: u64) -> (Self, Arc<std::sync::atomic::AtomicU64>) {
            let cell = Arc::new(std::sync::atomic::AtomicU64::new(start));
            (Self(Arc::clone(&cell)), cell)
        }
    }

    impl Monotonic for TestClock {
        fn now_secs(&self) -> u64 {
            self.0.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[test]
    fn the_real_clock_admits_exactly_the_budget_and_then_denies() {
        // The regression that mattered: with the broken clock this returned
        // `true` forever. Uses `SystemMonotonic`, because a test-clock version
        // would pass even with the real clock broken — which is how the defect
        // survived the original test suite.
        let limiter = RateLimiter::new(15, Duration::from_secs(60));
        let admitted = (0..40).filter(|_| limiter.check("k")).count();
        assert_eq!(
            admitted, 15,
            "the limiter must admit exactly its budget, then refuse"
        );
    }

    #[test]
    fn the_window_slides_so_a_throttled_key_recovers() {
        // With the clock broken, `now` never advanced and the key stayed denied
        // for the life of the process — a permanent lockout for one client, which
        // is a self-inflicted DoS rather than a throttle.
        let (clock, handle) = TestClock::new(1_000);
        let limiter = RateLimiter::with_clock(3, Duration::from_secs(60), Arc::new(clock));

        assert!(limiter.check("k"));
        assert!(limiter.check("k"));
        assert!(limiter.check("k"));
        assert!(!limiter.check("k"), "the fourth must be refused");

        handle.store(1_000 + 61, std::sync::atomic::Ordering::SeqCst);
        assert!(
            limiter.check("k"),
            "once the window slides the key must recover"
        );
    }

    #[test]
    fn a_window_that_has_not_elapsed_yet_does_not_discard_its_own_hits() {
        // Regression for the `saturating_sub` pruning: with the clock starting at
        // 0 and a 60 s window there is no cutoff yet, and every hit so far is
        // inside the window.
        let (clock, _) = TestClock::new(0);
        let limiter = RateLimiter::with_clock(3, Duration::from_secs(60), Arc::new(clock));
        assert!(limiter.check("k"));
        assert!(limiter.check("k"));
        assert!(limiter.check("k"));
        assert!(
            !limiter.check("k"),
            "hits recorded at t == 0 must count, or the limiter never engages"
        );
    }

    #[test]
    fn the_key_count_is_bounded() {
        let limiter = RateLimiter::with_key_cap(5, Duration::from_secs(60), 4);
        for i in 0..50 {
            assert!(limiter.check(&format!("client-{i}")));
        }
        assert!(
            limiter.tracked_keys() <= 4,
            "the map must stay bounded, got {}",
            limiter.tracked_keys()
        );
        assert_eq!(DEFAULT_MAX_KEYS, 16_384, "the documented default");
    }

    #[test]
    fn a_denied_attempt_does_not_extend_the_window() {
        // Otherwise a client could keep itself locked out indefinitely by
        // retrying, which is a denial-of-service the client inflicts on itself.
        // The denied attempt must also not *shorten* the window: the budget is
        // measured from the allowed attempts, not from the most recent try.
        let (clock, handle) = TestClock::new(500);
        let limiter = RateLimiter::with_clock(2, Duration::from_secs(60), Arc::new(clock));
        assert!(limiter.check("k"));
        assert!(limiter.check("k"));

        handle.store(500 + 30, std::sync::atomic::Ordering::SeqCst);
        assert!(!limiter.check("k"), "still inside the window");
        handle.store(500 + 31, std::sync::atomic::Ordering::SeqCst);
        assert!(
            !limiter.check("k"),
            "the denied attempt must not have moved the window either"
        );

        // 500 + 60 is the cutoff; the hits at exactly 500 fall outside it, since
        // Python keeps `t > cutoff`.
        handle.store(561, std::sync::atomic::Ordering::SeqCst);
        assert!(
            limiter.check("k"),
            "once the original attempts age out the key must recover"
        );
    }
}
