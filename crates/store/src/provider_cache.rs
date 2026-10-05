//! T0311: the provider-list cache, 30 s TTL over an injected clock.
//!
//! Ports the `_providers_cache` in `routes/model_routes.py:1910`. The reason it exists
//! is stated there and is worth keeping in view: `/api/providers` "runs a full host
//! port-scan (`discover_models`) which can take seconds when a configured LLM host is
//! unreachable. It's fetched on every page load, so cache it briefly."
//!
//! # This is the T0305 cache shape, not a second one
//!
//! T0305 built a TTL cache for the LM Studio probe and the brief was explicit: reuse
//! that shape rather than write a second, and if a third appeared, reshape instead.
//! So [`TtlCache`] below is deliberately the same three parts as
//! `LmStudioProbe`'s cache — an injected [`Clock`], a `expires_at > now` comparison,
//! and an injectable TTL — with the provider payload swapped in. The one thing that
//! differs from T0305 is that this cache has no transport to withhold on error, so it
//! has no "do not cache a failure" asymmetry to express; [`TtlCache::get_or_load`]
//! simply does not cache a loader error, which is the same rule applied to a case that
//! cannot currently produce one.
//!
//! If a future task needs a third TTL cache, the answer is to widen this one rather than
//! copy it.

use std::{cell::RefCell, time::Duration};

use crate::model_endpoints_capabilities::Clock;

/// The upstream TTL for the provider list.
pub const PROVIDERS_CACHE_TTL: Duration = Duration::from_secs(30);

/// A cached value with an absolute expiry.
///
/// The expiry is stored rather than a "fetched at" stamp so that the freshness test is
/// one comparison with no arithmetic at the read site, and so an entry cannot be
/// resurrected by a clock that moved backwards.
#[derive(Debug, Clone)]
struct Entry<V> {
    value: V,
    /// Absolute expiry in [`Clock::now_secs`] units.
    expires_at: u64,
}

/// A one-slot TTL cache.
///
/// One slot, not a map: the provider list is a single document, and Python's
/// `_providers_cache` is likewise a single `{"data", "time"}` pair. A map would invite
/// per-key TTLs nobody asked for.
///
/// Not `Sync` — it mutates through a `RefCell`. A caller sharing it across tasks wraps
/// it in a `Mutex`; see [`crate::oauth_device_flow::DeviceFlowStore`] for the same
/// reasoning applied to a SQLite-backed type.
pub struct TtlCache<V> {
    slot: RefCell<Option<Entry<V>>>,
    ttl: Duration,
}

impl<V> std::fmt::Debug for TtlCache<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TtlCache")
            .field("cached", &self.slot.borrow().is_some())
            .field("ttl", &self.ttl)
            .finish()
    }
}

impl<V: Clone> TtlCache<V> {
    /// An empty cache with an explicit TTL.
    #[must_use]
    pub const fn with_ttl(ttl: Duration) -> Self {
        Self {
            slot: RefCell::new(None),
            ttl,
        }
    }

    /// The cached value if it is still fresh.
    #[must_use]
    pub fn get(&self, clock: &dyn Clock) -> Option<V> {
        let slot = self.slot.borrow();
        // Strictly greater, so an entry is stale at exactly its expiry — the same
        // boundary T0305 pins.
        slot.as_ref()
            .filter(|e| e.expires_at > clock.now_secs())
            .map(|e| e.value.clone())
    }

    /// Stores `value`, arming the TTL from `clock`'s current time.
    pub fn put(&self, value: V, clock: &dyn Clock) {
        *self.slot.borrow_mut() = Some(Entry {
            value,
            expires_at: clock.now_secs().saturating_add(self.ttl.as_secs()),
        });
    }

    /// The cached value, or the result of `load`, caching a success.
    ///
    /// A loader error is **not** cached. T0305 needed that rule spelled out because it
    /// could distinguish a transport failure from a response; here every failure is
    /// exceptional, and caching one would keep serving a stale list for 30 s after the
    /// network came back. Not caching it means the next call retries.
    ///
    /// # Errors
    ///
    /// Propagates `load`'s error, having cached nothing.
    pub fn get_or_load<E>(
        &self,
        clock: &dyn Clock,
        load: impl FnOnce() -> Result<V, E>,
    ) -> Result<V, E> {
        if let Some(hit) = self.get(clock) {
            return Ok(hit);
        }
        let value = load()?;
        self.put(value.clone(), clock);
        Ok(value)
    }

    /// Empties the cache.
    pub fn clear(&self) {
        *self.slot.borrow_mut() = None;
    }

    /// Whether a fresh entry is held.
    #[must_use]
    pub fn is_fresh(&self, clock: &dyn Clock) -> bool {
        self.get(clock).is_some()
    }
}

/// One provider group in the `/providers` response.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProviderGroup {
    /// The provider name, e.g. `vllm` or `openai`.
    pub provider: Box<str>,
    /// The hosts that were scanned, when the group carries them.
    ///
    /// `None` rather than an empty list, because Python's `get_providers` includes
    /// `hosts` only on the scanned group (`model_discovery.py:270`) and omitting it on
    /// the static one is a real shape difference, not an empty result.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hosts: Option<Box<[Box<str>]>>,
    /// The endpoints found.
    pub items: Box<[serde_json::Value]>,
}

/// The cached `/providers` document.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProviderList {
    /// One group per provider.
    pub providers: Box<[ProviderGroup]>,
}

/// The provider-list cache, with the upstream 30 s TTL.
pub type ProvidersCache = TtlCache<ProviderList>;

/// A cache with the upstream TTL.
#[must_use]
pub fn providers_cache() -> ProvidersCache {
    TtlCache::with_ttl(PROVIDERS_CACHE_TTL)
}

/// The static OpenAI group `get_providers` appends when a key is configured.
///
/// Ports the literal list from `model_discovery.py:274-279`. Hard-coded upstream, and
/// kept as a literal rather than derived: inventing a model list here would be
/// fabricating capability data, which is exactly what the task forbids.
#[must_use]
pub fn openai_group() -> ProviderGroup {
    ProviderGroup {
        provider: "openai".into(),
        hosts: None,
        items: Box::new([serde_json::json!({
            "url": "https://api.openai.com/v1/chat/completions",
            "models": [
                "gpt-5.2-codex",
                "gpt-4o-mini",
                "gpt-image-1.5",
                "gpt-4o",
                "gpt-5.2",
                "gpt-5.2-pro",
            ]
        })]),
    }
}

/// Builds the response body.
///
/// Ports `get_providers` (`model_discovery.py:266`): the scanned group first, then the
/// static OpenAI group only when an API key is configured.
#[must_use]
pub fn build_provider_list(scanned: ProviderGroup, has_openai_key: bool) -> ProviderList {
    let mut providers = vec![scanned];
    if has_openai_key {
        providers.push(openai_group());
    }
    ProviderList {
        providers: providers.into_boxed_slice(),
    }
}

#[cfg(test)]
mod tests {
    use {
        super::{
            PROVIDERS_CACHE_TTL, ProviderGroup, ProviderList, build_provider_list, openai_group,
            providers_cache,
        },
        crate::model_endpoints_capabilities::TestClock,
        std::time::Duration,
    };

    fn scanned() -> ProviderGroup {
        ProviderGroup {
            provider: "vllm".into(),
            hosts: Some(vec![Box::from("localhost")].into_boxed_slice()),
            items: Box::new([serde_json::json!({"url": "http://x", "models": ["m"]})]),
        }
    }

    fn doc() -> ProviderList {
        build_provider_list(scanned(), false)
    }

    #[test]
    fn the_default_ttl_is_thirty_seconds() {
        assert_eq!(PROVIDERS_CACHE_TTL, Duration::from_secs(30));
    }

    #[test]
    fn a_cold_cache_loads_then_serves_from_cache() {
        let cache = providers_cache();
        let clock = TestClock::at(0);
        let calls = std::cell::Cell::new(0);
        for _ in 0..5 {
            let got = cache
                .get_or_load(&clock, || {
                    calls.set(calls.get() + 1);
                    Ok::<_, ()>(doc())
                })
                .expect("loads");
            assert_eq!(got, doc());
        }
        assert_eq!(calls.get(), 1, "only the first call loads");
    }

    #[test]
    fn an_entry_is_stale_at_exactly_the_ttl() {
        let cache = providers_cache();
        let clock = TestClock::at(0);
        cache.put(doc(), &clock);

        clock.advance(Duration::from_secs(29));
        assert!(cache.is_fresh(&clock), "29s in is inside a 30s TTL");

        clock.advance(Duration::from_secs(1));
        assert!(!cache.is_fresh(&clock), "the TTL boundary expires");
    }

    #[test]
    fn an_expired_entry_reloads() {
        let cache = providers_cache();
        let clock = TestClock::at(0);
        cache.put(doc(), &clock);
        clock.advance(Duration::from_secs(31));
        assert_eq!(cache.get(&clock), None);
        let got = cache
            .get_or_load(&clock, || Ok::<_, ()>(doc()))
            .expect("loads");
        assert_eq!(got, doc());
    }

    /// A loader failure must not be cached.
    ///
    /// Otherwise a transient scan failure keeps serving an empty list for 30 s after the
    /// network recovers, which reads as "no model servers found" rather than as an
    /// error.
    #[test]
    fn a_loader_failure_is_not_cached() {
        let cache = providers_cache();
        let clock = TestClock::at(0);
        let calls = std::cell::Cell::new(0);
        for _ in 0..3 {
            assert!(
                cache
                    .get_or_load(&clock, || {
                        calls.set(calls.get() + 1);
                        Err::<ProviderList, _>("scan failed")
                    })
                    .is_err()
            );
        }
        assert_eq!(calls.get(), 3, "every attempt retried");
    }

    #[test]
    fn a_refresh_bypasses_a_fresh_entry() {
        let cache = providers_cache();
        let clock = TestClock::at(0);
        cache.put(doc(), &clock);
        assert!(cache.is_fresh(&clock));
        // `refresh=true` in the Python skips the read entirely and re-scans.
        cache.clear();
        assert!(!cache.is_fresh(&clock));
    }

    #[test]
    fn the_openai_group_appears_only_with_a_key() {
        assert_eq!(build_provider_list(scanned(), false).providers.len(), 1);
        let with_key = build_provider_list(scanned(), true);
        assert_eq!(with_key.providers.len(), 2);
        assert_eq!(with_key.providers[1].provider.as_ref(), "openai");
        // The static group carries no `hosts` key at all.
        assert!(with_key.providers[1].hosts.is_none());
    }

    #[test]
    fn the_openai_group_lists_the_upstream_models() {
        let group = openai_group();
        let models = group.items[0]["models"].as_array().expect("array");
        let ids: Vec<&str> = models.iter().filter_map(|m| m.as_str()).collect();
        assert_eq!(
            ids,
            [
                "gpt-5.2-codex",
                "gpt-4o-mini",
                "gpt-image-1.5",
                "gpt-4o",
                "gpt-5.2",
                "gpt-5.2-pro",
            ]
        );
    }

    /// The scanned group leads, matching Python's ordering.
    #[test]
    fn the_scanned_group_comes_first() {
        let list = build_provider_list(scanned(), true);
        assert_eq!(list.providers[0].provider.as_ref(), "vllm");
        assert_eq!(list.providers[0].hosts.as_ref().expect("present").len(), 1);
    }

    #[test]
    fn the_group_serialises_without_an_empty_hosts_key() {
        let rendered = serde_json::to_string(&openai_group()).expect("serialises");
        assert!(!rendered.contains("hosts"), "{rendered}");
        let scanned_rendered = serde_json::to_string(&scanned()).expect("serialises");
        assert!(scanned_rendered.contains("hosts"), "{scanned_rendered}");
    }

    #[test]
    fn a_cached_document_survives_a_round_trip_through_json() {
        let cache = providers_cache();
        let clock = TestClock::at(0);
        cache.put(doc(), &clock);
        let hit = cache.get(&clock).expect("fresh");
        let json = serde_json::to_string(&hit).expect("serialises");
        let back: ProviderList = serde_json::from_str(&json).expect("deserialises");
        assert_eq!(back, hit);
    }
}
