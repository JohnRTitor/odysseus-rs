//! Integrity and validity tests for the captured ChromaDB vector golden set.
//!
//! These are **not** retrieval tests. Nothing here runs a vector index — the
//! vector port does not exist yet, and building one is out of scope for the
//! capture. What these tests guard is the thing that cannot be regenerated: the
//! fixture's own internal consistency, its provenance, and its determinism.
//!
//! The most valuable test here is
//! [`recorded_distances_agree_with_closed_form_cosine`]. It re-derives every
//! recorded distance from the corpus embeddings in the fixture, using cosine
//! geometry, and asserts agreement within the noise floor the capture itself
//! measured. That means the fixture is self-verifying: if a future edit breaks
//! the numbers, the test fails even though nothing about the file's *shape*
//! changed. It also pins the metric — the identity queries only come out at
//! distance ~0 under cosine.

use std::collections::{BTreeMap, BTreeSet};

use {
    capture_golden_set::{
        DIMENSION, QUERY_COUNT, Query, QueryKind, SEED, TOP_K, Vector, build_queries,
        cosine_distance,
    },
    serde::Deserialize,
};

/// The fixture. `include_str!` so a missing or renamed fixture is a compile
/// error rather than a runtime surprise in CI.
const FIXTURE: &str = include_str!("../../../docs/baseline/vector_golden_set.json");

/// Names of the collections the app creates, one per vector lane.
const EXPECTED_COLLECTIONS: [&str; 3] = [
    "odysseus_memories_fastembed",
    "odysseus_rag_fastembed",
    "odysseus_tool_index_fastembed",
];

/// The only collection that held records at capture time.
const SEED_COLLECTION: &str = "odysseus_tool_index_fastembed";

// ---------------------------------------------------------------- mirror types

#[derive(Debug, Deserialize)]
struct Fixture {
    schema_version: u32,
    captured_at_utc: String,
    generator: Generator,
    source: Source,
    metric: Metric,
    embedding: Embedding,
    collections: Vec<CollectionRecord>,
    corpus: Vec<CorpusRecord>,
    queries: Vec<QueryRecord>,
}

#[derive(Debug, Deserialize)]
struct Generator {
    prng: String,
    seed: String,
    seed_collection: String,
    query_count: usize,
    top_k: usize,
}

#[derive(Debug, Deserialize)]
struct Source {
    chroma_version: String,
    api: String,
}

#[derive(Debug, Deserialize)]
struct Metric {
    declared: String,
    set_by: String,
    distance_convention: String,
    similarity_convention: String,
    both_recorded: bool,
    vectors_normalised_on_insert: bool,
    noise_floor: NoiseFloor,
}

#[derive(Debug, Deserialize)]
struct NoiseFloor {
    samples: usize,
    median_abs: f64,
    p99_abs: f64,
    max_abs: f64,
}

#[derive(Debug, Deserialize)]
struct Embedding {
    model: String,
    dimension: usize,
    fingerprint: String,
    lane: String,
}

#[derive(Debug, Deserialize)]
struct CollectionRecord {
    name: String,
    id: String,
    record_count: usize,
    empty_at_capture: bool,
    hnsw_space: Option<String>,
    embedding_dimension: Option<i64>,
    embedding_fingerprint: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CorpusRecord {
    id: String,
    document: Option<String>,
    metadata: BTreeMap<String, serde_json::Value>,
    vector: Vector,
}

#[derive(Debug, Deserialize)]
struct QueryRecord {
    id: String,
    kind: QueryKind,
    text: String,
    vector: Vector,
    results: Vec<QueryCollectionResult>,
}

#[derive(Debug, Deserialize)]
struct QueryCollectionResult {
    collection: String,
    status: String,
    hits: Vec<Hit>,
}

#[derive(Debug, Deserialize)]
struct Hit {
    rank: usize,
    id: String,
    distance: f64,
    similarity: f64,
}

// ---------------------------------------------------------------- helpers

fn fixture() -> Fixture {
    serde_json::from_str(FIXTURE).expect("golden set must parse")
}

fn norm(v: &[f32]) -> f64 {
    f64::from(capture_golden_set::l2_norm(v))
}

/// Results for one collection across every query.
fn per_collection<'f>(f: &'f Fixture, name: &str) -> Vec<&'f QueryCollectionResult> {
    f.queries
        .iter()
        .map(|q| {
            q.results
                .iter()
                .find(|r| r.collection == name)
                .unwrap_or_else(|| panic!("{} missing collection {name}", q.id))
        })
        .collect()
}

// ---------------------------------------------------------------- provenance

#[test]
fn fixture_is_the_captured_artifact_not_a_placeholder() {
    let f = fixture();
    assert_eq!(
        f.schema_version, 1,
        "bump the reader when the layout changes"
    );
    assert!(
        f.captured_at_utc.ends_with('Z') && f.captured_at_utc.len() == 20,
        "captured_at_utc must be a UTC timestamp, got {:?}",
        f.captured_at_utc
    );
    assert!(
        f.source.chroma_version.starts_with("1."),
        "unexpected Chroma version"
    );
    assert!(
        f.source.api.contains("/tenants/"),
        "the API path must record the tenant segment"
    );
}

/// The single most important line in the fixture. `SPIKE_RESULTS.md` S7 claimed
/// ChromaDB's default metric matches sqlite-vec's default (L2); it does not.
/// Cosine is set explicitly by the Python client, so a port that takes
/// sqlite-vec's default silently changes every calibrated threshold.
#[test]
fn metric_is_declared_cosine_and_both_conventions_are_recorded() {
    let f = fixture();
    assert_eq!(
        f.metric.declared, "cosine",
        "the metric must be declared, not implied"
    );
    assert!(
        f.metric.both_recorded,
        "raw distance AND derived similarity must both be stored"
    );
    assert!(
        f.metric
            .distance_convention
            .contains("1 - cosine_similarity"),
        "the distance convention must be documented in the fixture"
    );
    assert_eq!(
        f.metric.similarity_convention,
        "similarity = 1.0 - distance"
    );
    assert!(
        f.metric.vectors_normalised_on_insert,
        "normalisation is load-bearing for cosine"
    );
    assert!(
        f.metric.set_by.contains("embedding_lanes.py"),
        "the fixture must cite where the metric was set"
    );
}

#[test]
fn embedding_identity_is_recorded_so_a_lane_swap_is_detectable() {
    let f = fixture();
    assert_eq!(f.embedding.dimension, DIMENSION);
    assert_eq!(f.embedding.model, "sentence-transformers/all-MiniLM-L6-v2");
    assert_eq!(f.embedding.lane, "fastembed");
    assert_eq!(
        f.embedding.fingerprint, "9d32c541ea92982d",
        "fingerprint changed: the lane was re-embedded, so this fixture no longer describes the live data"
    );
}

// ---------------------------------------------------------------- collections

#[test]
fn all_three_collections_are_recorded_including_the_empty_ones() {
    let f = fixture();
    let names: BTreeSet<&str> = f.collections.iter().map(|c| c.name.as_str()).collect();
    let expected: BTreeSet<&str> = EXPECTED_COLLECTIONS.into_iter().collect();
    assert_eq!(names, expected, "every lane's collection must be present");

    for c in &f.collections {
        assert!(!c.id.is_empty(), "{} has no id", c.name);
        assert_eq!(
            c.hnsw_space.as_deref(),
            Some("cosine"),
            "{} is not cosine; the whole golden set is invalid",
            c.name
        );
        assert_eq!(c.embedding_dimension, Some(DIMENSION as i64), "{}", c.name);
        assert_eq!(
            c.embedding_fingerprint.as_deref(),
            Some("9d32c541ea92982d"),
            "{} was built by a different embedding lane; the corpus vectors would not be comparable",
            c.name
        );
    }
}

/// Two of the three collections were empty at capture time. A fixture that
/// silently omitted them would look identical to one where they had data, which
/// is precisely the difference that matters when someone replays it.
#[test]
fn empty_collections_are_recorded_as_empty_not_omitted() {
    let f = fixture();
    let mut empty: Vec<&str> = f
        .collections
        .iter()
        .filter(|c| c.empty_at_capture)
        .map(|c| c.name.as_str())
        .collect();
    empty.sort_unstable();
    assert_eq!(
        empty,
        vec!["odysseus_memories_fastembed", "odysseus_rag_fastembed"]
    );

    let seeded: Vec<&CollectionRecord> = f
        .collections
        .iter()
        .filter(|c| !c.empty_at_capture)
        .collect();
    assert_eq!(
        seeded.len(),
        1,
        "exactly one collection held data at capture"
    );
    let seeded = seeded[0];
    assert_eq!(seeded.name, SEED_COLLECTION);
    assert_eq!(seeded.record_count, 69);
    assert_eq!(
        seeded.record_count,
        f.corpus.len(),
        "corpus must hold every record"
    );

    for name in empty {
        let rows = per_collection(&f, name);
        assert_eq!(
            rows.len(),
            QUERY_COUNT,
            "{name} must appear for every query"
        );
        for r in rows {
            assert_eq!(r.status, "empty_collection_at_capture", "{}", name);
            assert!(
                r.hits.is_empty(),
                "{name} must have zero hits, got {}",
                r.hits.len()
            );
        }
    }
}

#[test]
fn corpus_is_complete_and_normalised() {
    let f = fixture();
    assert_eq!(f.corpus.len(), 69);
    let ids: BTreeSet<&str> = f.corpus.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids.len(), f.corpus.len(), "corpus ids must be unique");
    for r in &f.corpus {
        assert_eq!(
            r.vector.as_slice().len(),
            DIMENSION,
            "{} has wrong dimension",
            r.id
        );
        assert!(
            (norm(r.vector.as_slice()) - 1.0).abs() < 1e-5,
            "{} is not unit-length",
            r.id
        );
        assert!(r.document.is_some(), "{} has no document text", r.id);
        assert!(
            r.metadata.contains_key("tool_name"),
            "{} has no tool_name",
            r.id
        );
    }
}

// ---------------------------------------------------------------- queries

#[test]
fn there_are_two_hundred_queries_with_stable_ids() {
    let f = fixture();
    assert_eq!(f.generator.query_count, QUERY_COUNT);
    assert_eq!(f.generator.top_k, TOP_K);
    assert_eq!(f.generator.prng, "SplitMix64");
    assert_eq!(
        f.generator.seed,
        format!("0x{SEED:016X}"),
        "seed must match the generator"
    );
    assert_eq!(f.generator.seed_collection, SEED_COLLECTION);

    assert_eq!(f.queries.len(), QUERY_COUNT);
    let ids: BTreeSet<&str> = f.queries.iter().map(|q| q.id.as_str()).collect();
    assert_eq!(ids.len(), QUERY_COUNT, "query ids must be unique");
    assert_eq!(f.queries[0].id, "q000");
    assert_eq!(f.queries[QUERY_COUNT - 1].id, "q199");
}

/// Determinism, proven against the fixture rather than against a second call in
/// the same process: rebuilding the query set from the fixture's own corpus must
/// reproduce the fixture's query vectors exactly. If this fails, the fixture was
/// generated from unseeded randomness and cannot fail a build on a behaviour
/// change — which is the whole point of a golden set.
#[test]
fn query_vectors_are_reproducible_from_the_seed_and_the_corpus() {
    let f = fixture();
    let corpus: Vec<Vec<f32>> = f.corpus.iter().map(|r| r.vector.0.clone()).collect();
    let rebuilt: Vec<Query> = build_queries(&corpus);

    assert_eq!(rebuilt.len(), f.queries.len());
    for (expected, got) in rebuilt.iter().zip(&f.queries) {
        assert_eq!(expected.id, got.id, "query order must be stable");
        assert_eq!(expected.kind, got.kind, "{} kind", got.id);
        assert_eq!(
            expected.vector,
            got.vector.as_slice(),
            "{}: rebuilt vector differs from the fixture; the capture is not reproducible",
            got.id
        );
    }
}

#[test]
fn identity_queries_are_verbatim_corpus_vectors() {
    let f = fixture();
    let identities: Vec<&QueryRecord> = f
        .queries
        .iter()
        .filter(|q| q.kind == QueryKind::Identity)
        .collect();
    assert_eq!(identities.len(), f.corpus.len());
    for (q, row) in identities.iter().zip(&f.corpus) {
        assert_eq!(
            q.vector.as_slice(),
            row.vector.as_slice(),
            "{} must be {} verbatim",
            q.id,
            row.id
        );
    }
    let blends = f
        .queries
        .iter()
        .filter(|q| q.kind == QueryKind::Blend)
        .count();
    assert_eq!(
        blends,
        QUERY_COUNT - f.corpus.len(),
        "the rest must be blends"
    );
}

#[test]
fn every_query_vector_is_a_unit_384_vector() {
    let f = fixture();
    for q in &f.queries {
        assert_eq!(q.vector.as_slice().len(), DIMENSION, "{}", q.id);
        assert!(
            (norm(q.vector.as_slice()) - 1.0).abs() < 1e-5,
            "{} is not unit-length",
            q.id
        );
        assert!(!q.text.is_empty(), "{} has no provenance note", q.id);
    }
}

// ---------------------------------------------------------------- hits

#[test]
fn every_query_has_a_result_row_for_every_collection() {
    let f = fixture();
    for q in &f.queries {
        let names: BTreeSet<&str> = q.results.iter().map(|r| r.collection.as_str()).collect();
        let expected: BTreeSet<&str> = EXPECTED_COLLECTIONS.into_iter().collect();
        assert_eq!(names, expected, "{} must cover every collection", q.id);
    }
}

#[test]
fn seeded_collection_returns_top_ten_ranked_and_sorted() {
    let f = fixture();
    let rows = per_collection(&f, SEED_COLLECTION);
    for r in &rows {
        assert_eq!(r.status, "ok");
        assert_eq!(r.hits.len(), TOP_K, "expected top-{TOP_K}");
        for (i, hit) in r.hits.iter().enumerate() {
            assert_eq!(hit.rank, i + 1, "ranks must be 1..={TOP_K} contiguous");
        }
        for pair in r.hits.windows(2) {
            assert!(
                pair[0].distance <= pair[1].distance,
                "hits must be ordered by ascending distance: {} then {}",
                pair[0].distance,
                pair[1].distance
            );
        }
    }
}

#[test]
fn every_hit_id_exists_in_the_captured_corpus() {
    let f = fixture();
    let known: BTreeSet<&str> = f.corpus.iter().map(|r| r.id.as_str()).collect();
    for r in per_collection(&f, SEED_COLLECTION) {
        for hit in &r.hits {
            assert!(known.contains(hit.id.as_str()), "unknown hit id {}", hit.id);
        }
    }
}

/// The convention check, on every single row. If a port or a refactor ever starts
/// writing `distance` where `similarity` belongs — or vice versa — this is what
/// catches it, and the failure names the query and rank.
///
/// The tolerance is 1 ULP of `f64` (~2.2e-16), not equality, and that is
/// deliberate. `similarity` was computed as `1.0 - distance` in the generator and
/// then serialised; reading it back means parsing decimal text, and a JSON parser
/// without `float_roundtrip` is not correctly rounded — it can land one ULP away.
/// So bit-exact equality is not a property this file can have across a text
/// round-trip, and pretending otherwise would make the assertion flaky rather
/// than strict. One ULP is nine orders of magnitude below the measured noise
/// floor, so this still catches any real convention break.
#[test]
fn similarity_is_one_minus_distance_on_every_row() {
    let f = fixture();
    let tolerance = f64::EPSILON;
    let mut checked = 0usize;
    for (qi, r) in per_collection(&f, SEED_COLLECTION).iter().enumerate() {
        for hit in &r.hits {
            let recomputed = 1.0 - hit.distance;
            assert!(
                (hit.similarity - recomputed).abs() <= tolerance,
                "{} rank {} ({}): similarity {:?} is not 1.0 - distance {:?} = {:?}",
                f.queries[qi].id,
                hit.rank,
                hit.id,
                hit.similarity,
                hit.distance,
                recomputed
            );
            assert!(
                (-1e-6..=2.0).contains(&hit.distance),
                "cosine distance out of range: {}",
                hit.distance
            );
            assert!(
                hit.similarity <= 1.0 + 1e-6,
                "similarity above 1: {}",
                hit.similarity
            );
            checked += 1;
        }
    }
    assert_eq!(
        checked,
        QUERY_COUNT * TOP_K,
        "expected {QUERY_COUNT} x {TOP_K} rows"
    );
}

/// The self-verification test. Every recorded distance is re-derived from the
/// fixture's own corpus embeddings with cosine geometry, and required to agree
/// within the noise floor the capture measured.
///
/// This is what makes the fixture worth more than a table of numbers: it can be
/// checked against mathematics as well as against the recording. It also pins the
/// metric — under L2 on these normalised vectors the identity queries would come
/// out at distance ~0.38 rather than ~0, and this would fail.
#[test]
fn recorded_distances_agree_with_closed_form_cosine() {
    let f = fixture();
    let by_id: BTreeMap<&str, &[f32]> = f
        .corpus
        .iter()
        .map(|r| (r.id.as_str(), r.vector.as_slice()))
        .collect();
    let tolerance = f.metric.noise_floor.max_abs * 2.0 + 1e-9;

    let mut worst = 0.0f64;
    let mut worst_where = String::new();
    for (qi, q) in f.queries.iter().enumerate() {
        let row = &per_collection(&f, SEED_COLLECTION)[qi];
        for hit in &row.hits {
            let vector = by_id[hit.id.as_str()];
            let recomputed = cosine_distance(q.vector.as_slice(), vector);
            let delta = (recomputed - hit.distance).abs();
            if delta > worst {
                worst = delta;
                worst_where = format!("{} rank {} ({})", q.id, hit.rank, hit.id);
            }
        }
    }

    assert!(
        worst <= tolerance,
        "recorded distances disagree with closed-form cosine by {worst:.3e} at {worst_where}, \
         above the captured noise floor of {:.3e}. Either the metric is not cosine, or the \
         query and corpus vectors are not in the same space.",
        f.metric.noise_floor.max_abs
    );
}

/// Identity queries must retrieve themselves at distance ~0. A tolerance rather
/// than equality, because Chroma computes in f32 and can return a tiny negative
/// distance for a self-match.
#[test]
fn identity_queries_retrieve_themselves_at_distance_zero() {
    let f = fixture();
    let rows = per_collection(&f, SEED_COLLECTION);
    let mut checked = 0;
    for (q, r) in f.queries.iter().zip(&rows) {
        if q.kind != QueryKind::Identity {
            continue;
        }
        let expected = f
            .corpus
            .iter()
            .find(|c| c.vector.0 == q.vector.0)
            .map(|c| c.id.as_str());
        let Some(expected) = expected else { continue };
        assert_eq!(
            r.hits[0].id, expected,
            "{} is an identity query for {expected} but rank 1 is {}",
            q.id, r.hits[0].id
        );
        assert!(
            r.hits[0].distance.abs() < 1e-6,
            "{}: self-match distance {} is not ~0",
            q.id,
            r.hits[0].distance
        );
        assert!(
            r.hits[0].similarity > 0.999_999,
            "{}: self similarity too low",
            q.id
        );
        checked += 1;
    }
    assert!(
        checked >= 69,
        "expected at least one identity query per document, got {checked}"
    );
}

/// Distances must span a real range. A fixture where every query returns the same
/// ordering cannot discriminate between a good port and a broken one.
#[test]
fn recorded_distances_span_a_discriminating_range() {
    let f = fixture();
    let distances: Vec<f64> = per_collection(&f, SEED_COLLECTION)
        .iter()
        .flat_map(|r| r.hits.iter().map(|h| h.distance))
        .collect();
    assert_eq!(distances.len(), QUERY_COUNT * TOP_K);

    let min = distances.iter().copied().fold(f64::MAX, f64::min);
    let max = distances.iter().copied().fold(f64::MIN, f64::max);
    assert!(
        min < 0.01,
        "no near-matches: the set cannot test a good port (min {min})"
    );
    assert!(max > 0.5, "distances are compressed at the top (max {max})");

    let distinct: BTreeSet<u64> = distances.iter().map(|d| d.to_bits()).collect();
    assert!(
        distinct.len() > 1_000,
        "only {} distinct distances in the set",
        distinct.len()
    );
}

/// The noise floor is a measurement, so it gets checked like one. A port replaying
/// this set can never beat f32-vs-f64 arithmetic, and recording the number is
/// what stops a future engineer chasing a discrepancy smaller than it.
#[test]
fn noise_floor_is_recorded_and_physically_plausible() {
    let f = fixture();
    let n = &f.metric.noise_floor;
    assert_eq!(
        n.samples,
        QUERY_COUNT * TOP_K,
        "every row must be cross-checked"
    );
    assert!(
        n.median_abs <= n.p99_abs && n.p99_abs <= n.max_abs,
        "floor must be ordered"
    );
    assert!(
        n.max_abs < 1e-5,
        "noise floor {} is too large to be f32 rounding; the vectors may be truncated",
        n.max_abs
    );
    assert!(
        n.median_abs > 0.0,
        "an exactly zero floor means the cross-check did not run"
    );
}
