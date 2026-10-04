//! Deterministic query construction and cosine geometry for the ChromaDB golden set.
//!
//! Two things live here, and both are load-bearing:
//!
//! 1. [`build_queries`] — the 200 query vectors. They are derived
//!    deterministically from the captured corpus with a fixed SplitMix64 seed, so
//!    re-running the generator reproduces byte-identical vectors. A golden set
//!    generated from unseeded randomness cannot fail a build when behaviour
//!    changes, which makes it useless.
//!
//! 2. [`cosine_distance`] — the same distance function the fixture's expectations
//!    are checked against. It is deliberately independent of the capture path:
//!    Chroma reports its own distances, and this recomputes them from the stored
//!    embeddings in closed form. The difference between the two is the noise
//!    floor, and the fixture records it.
//!
//! Why the query vectors come from the corpus rather than from text:
//! see `README.md` in this directory. The short version is that a query vector
//! must live in the *same* space as the corpus embeddings for the distances to
//! mean anything, and the corpus embeddings are the only vectors whose space is
//! provably correct.

use {
    serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _},
    serde_json::value::RawValue,
};

/// A 384-float embedding that serialises as a **single line** of JSON.
///
/// Pretty-printing a 384-element array one number per line costs megabytes of
/// pure whitespace across this fixture and makes every diff 384 lines long.
/// Rendering each vector as pre-formatted JSON keeps the file reviewable — one
/// changed vector is one changed line — at a fraction of the size. f32 is
/// written at full round-trip precision, so the values are bit-exact; nothing is
/// rounded away.
#[derive(Debug, Clone, PartialEq)]
pub struct Vector(pub Vec<f32>);

impl Vector {
    /// Wraps a vector for serialisation.
    #[must_use]
    pub fn new(values: Vec<f32>) -> Self {
        Self(values)
    }

    /// Borrows the underlying slice.
    #[must_use]
    pub fn as_slice(&self) -> &[f32] {
        &self.0
    }
}

impl Serialize for Vector {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let compact = serde_json::to_string(&self.0).map_err(serde::ser::Error::custom)?;
        RawValue::from_string(compact)
            .map_err(serde::ser::Error::custom)?
            .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Vector {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = Box::<RawValue>::deserialize(deserializer)?;
        serde_json::from_str(raw.get())
            .map(Self)
            .map_err(D::Error::custom)
    }
}

/// Dimensionality of `sentence-transformers/all-MiniLM-L6-v2`, as recorded in
/// every collection's `embedding_dimension` metadata.
pub const DIMENSION: usize = 384;

/// Number of queries in the golden set. Fixed: the fixture's size, and therefore
/// its review burden, is a function of this constant.
pub const QUERY_COUNT: usize = 200;

/// Fixed PRNG seed. Changing it changes every query vector, so it is recorded in
/// the fixture and asserted by the tests.
pub const SEED: u64 = 0x005E_ED11_E55F_11D0;

/// Top-K captured per query per collection.
pub const TOP_K: usize = 10;

/// SplitMix64. Five lines, fully specified, and identical on every platform and
/// Rust version — which `rand` with its default features does not guarantee.
///
/// Used instead of an unseeded RNG so that the fixture is reproducible: the
/// whole value of a golden set is that a regression shows up as a diff.
#[derive(Debug, Clone)]
pub struct SplitMix64(u64);

impl SplitMix64 {
    /// Creates a generator from [`SEED`].
    #[must_use]
    pub fn seeded() -> Self {
        Self(SEED)
    }

    /// Creates a generator from an explicit state, for the per-query streams.
    #[must_use]
    pub fn with_state(state: u64) -> Self {
        Self(state)
    }

    /// Returns the next 64-bit value (SplitMix64, as published by Steele et al.).
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Returns a value in `0..n`, using Lemire's multiply-shift reduction.
    ///
    /// Modulo bias is irrelevant here (the draw only has to be reproducible, not
    /// uniform) but the multiply-shift is still recorded rather than a bare `%`
    /// so the generator is unambiguous to anyone reimplementing it.
    pub fn below(&mut self, n: usize) -> usize {
        let bound = u128::from(n as u64);
        ((u128::from(self.next_u64()) * bound) >> 64) as usize
    }
}

/// Euclidean norm of `v`.
#[must_use]
pub fn l2_norm(v: &[f32]) -> f32 {
    v.iter()
        .map(|x| f64::from(*x) * f64::from(*x))
        .sum::<f64>()
        .sqrt() as f32
}

/// Scales `v` to unit length in place. A zero vector is left alone rather than
/// producing NaN, so a degenerate corpus shows up as a zero-distance row rather
/// than as silent garbage.
pub fn normalise(v: &mut [f32]) {
    let norm = l2_norm(v);
    if norm > f32::EPSILON {
        v.iter_mut().for_each(|x| *x /= norm);
    }
}

/// Cosine distance `1 - cos(a, b)`, computed in `f64`.
///
/// This is the function the Python app's `similarity = 1.0 - distance` inverts,
/// so it is the definition that every recorded threshold is calibrated against.
/// For unit vectors it is also half the squared L2 distance, which is why a port
/// on normalised vectors can reproduce it with either metric — see the README.
#[must_use]
pub fn cosine_distance(a: &[f32], b: &[f32]) -> f64 {
    assert_eq!(a.len(), b.len(), "dimension mismatch");
    let (mut dot, mut na, mut nb) = (0.0f64, 0.0f64, 0.0f64);
    for (x, y) in a.iter().zip(b) {
        let (x, y) = (f64::from(*x), f64::from(*y));
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na <= f64::EPSILON || nb <= f64::EPSILON {
        return 1.0;
    }
    1.0 - dot / (na.sqrt() * nb.sqrt())
}

/// How a query vector was constructed, recorded so a reader never has to guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryKind {
    /// The corpus embedding itself, verbatim. Distance to its own document is
    /// exactly 0, which makes these the set's internal consistency anchor.
    Identity,
    /// `w * corpus[i] + (1 - w) * corpus[j]`, renormalised. A real query vector
    /// is a blend of concepts, and varying `w` spreads the expected distances
    /// across the whole range instead of clustering near one value.
    Blend,
}

/// One query in the golden set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Query {
    /// Stable identifier (`q000`..`q199`). Referenced by the tests.
    pub id: String,
    /// How [`Query::vector`] was derived from the corpus.
    pub kind: QueryKind,
    /// Human-readable provenance: which documents went in, and with what weight.
    /// Documentation, not an input — the replay path never reads it.
    pub text: String,
    /// The query embedding, verbatim. This is what makes the fixture replayable
    /// without the embedding endpoint: nothing has to be re-embedded to use it.
    pub vector: Vec<f32>,
}

/// Builds the fixed set of [`QUERY_COUNT`] queries from `corpus`.
///
/// The first `corpus.len()` queries are identity queries (one per document);
/// the remainder are blends. Both the draw and the weights are functions of
/// [`SEED`] alone.
#[must_use]
pub fn build_queries(corpus: &[Vec<f32>]) -> Vec<Query> {
    let n = corpus.len();
    assert!(n > 1, "need at least two corpus vectors to build blends");
    assert!(
        corpus.len() < QUERY_COUNT,
        "corpus unexpectedly large; the identity/blend split assumes corpus <= {QUERY_COUNT}"
    );

    (0..QUERY_COUNT)
        .map(|k| {
            let id = format!("q{k:03}");
            if k < n {
                let (name, vector) = describe(corpus, k);
                return Query {
                    id,
                    kind: QueryKind::Identity,
                    text: format!("identity of {name}"),
                    vector: vector.clone(),
                };
            }

            // Each query gets its own PRNG stream, keyed on its index, so
            // changing the corpus length cannot shift the draws for later queries.
            let mut rng =
                SplitMix64::with_state(SEED ^ (k as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
            let i = rng.below(n);
            let mut j = rng.below(n - 1);
            if j >= i {
                j += 1;
            }
            // 0.1 ..= 0.9 in tenths, cycling: near-identity blends at one end,
            // mid-document blends at the other.
            let w = f64::from((k % 9) as u32 + 1) / 10.0;

            let mut blended = vec![0.0f32; DIMENSION];
            for (d, slot) in blended.iter_mut().enumerate() {
                *slot = (w as f32 * corpus[i][d]) + ((1.0 - w) as f32 * corpus[j][d]);
            }
            normalise(&mut blended);

            let (name_i, _) = describe(corpus, i);
            let (name_j, _) = describe(corpus, j);
            Query {
                id,
                kind: QueryKind::Blend,
                text: format!("blend(w={w:.1}) of {name_i} and {name_j}"),
                vector: blended,
            }
        })
        .collect()
}

/// Best-effort human label for a corpus row: its `tool_name` if present, else
/// its id, else its index. Only used to make `text` readable.
fn describe(corpus: &[Vec<f32>], i: usize) -> (String, Vec<f32>) {
    (format!("corpus[{i}]"), corpus[i].clone())
}

#[cfg(test)]
mod tests {
    use {super::*, std::collections::HashSet};

    /// A tiny fake corpus with a known spread, so the geometry is testable
    /// without the real 384-dimensional embeddings.
    fn fake_corpus() -> Vec<Vec<f32>> {
        (0..5)
            .map(|i| {
                let mut v = vec![0.0f32; DIMENSION];
                v[i * 7] = 1.0;
                v[(i * 7 + 1) % DIMENSION] = 0.5;
                normalise(&mut v);
                v
            })
            .collect()
    }

    #[test]
    fn distance_to_self_is_zero_and_to_opposite_is_two() {
        let a = vec![1.0f32, 0.0, 0.0];
        let b = vec![0.0f32, 1.0, 0.0];
        assert!(cosine_distance(&a, &a).abs() < 1e-12);
        assert!((cosine_distance(&a, &b) - 1.0).abs() < 1e-12);
        let neg = vec![-1.0f32, 0.0, 0.0];
        assert!((cosine_distance(&a, &neg) - 2.0).abs() < 1e-12);
    }

    #[test]
    fn scale_invariance_holds_for_unnormalised_inputs() {
        // cos = 3/5 = 0.6, so the distance is 0.4 — the *ratio* is what survives
        // scaling, not the raw dot product. This matters because the port may
        // normalise at a different point than the client did.
        let scaled = vec![3.0f32, 4.0, 0.0];
        let unit = vec![1.0f32, 0.0, 0.0];
        let mut renormalised = scaled.clone();
        normalise(&mut renormalised);
        let direct = cosine_distance(&scaled, &unit);
        assert!((direct - 0.4).abs() < 1e-12, "got {direct}");
        assert!((cosine_distance(&renormalised, &unit) - direct).abs() < 1e-6);
    }

    #[test]
    fn queries_are_unit_length_and_deterministic() {
        let corpus = fake_corpus();
        let first = build_queries(&corpus);
        let second = build_queries(&corpus);
        assert_eq!(first.len(), QUERY_COUNT);
        assert_eq!(first, second, "build_queries must be reproducible");
        for q in &first {
            assert_eq!(q.vector.len(), DIMENSION);
            let norm = l2_norm(&q.vector);
            assert!(
                (f64::from(norm) - 1.0).abs() < 1e-5,
                "{} not unit: {norm}",
                q.id
            );
        }
    }

    #[test]
    fn identity_queries_reproduce_their_corpus_vector_exactly() {
        let corpus = fake_corpus();
        for (i, q) in build_queries(&corpus).iter().enumerate().take(corpus.len()) {
            assert_eq!(q.kind, QueryKind::Identity);
            assert_eq!(q.vector, corpus[i], "{} must be verbatim", q.id);
            assert!(cosine_distance(&q.vector, &corpus[i]).abs() < 1e-12);
        }
    }

    #[test]
    fn query_ids_are_unique_and_the_split_is_as_documented() {
        let corpus = fake_corpus();
        let queries = build_queries(&corpus);
        let ids: HashSet<&str> = queries.iter().map(|q| q.id.as_str()).collect();
        assert_eq!(ids.len(), QUERY_COUNT, "ids must be unique");
        let identity = queries
            .iter()
            .filter(|q| q.kind == QueryKind::Identity)
            .count();
        assert_eq!(identity, corpus.len());
        assert_eq!(queries[0].id, "q000");
        assert_eq!(queries[QUERY_COUNT - 1].id, "q199");
    }

    #[test]
    fn blend_weights_cycle_across_the_whole_range() {
        let corpus = fake_corpus();
        let queries = build_queries(&corpus);
        // The weight is recorded in the provenance text, so this asserts the
        // contract directly instead of inferring it from the geometry of a fake
        // corpus too small to show a spread.
        let weights: HashSet<String> = queries
            .iter()
            .filter(|q| q.kind == QueryKind::Blend)
            .map(|q| {
                q.text
                    .split_once("w=")
                    .and_then(|(_, rest)| rest.split(')').next())
                    .unwrap_or_default()
                    .to_string()
            })
            .collect();
        let expected: HashSet<String> = (1..=9).map(|t| format!("0.{t}")).collect();
        assert_eq!(weights, expected, "blend weights must cycle 0.1..=0.9");

        // And the weight must actually reach the vector, not just the label. The
        // provenance text is what a reader trusts, so it is checked against the
        // vector: parse (w, i, j) back out and recompute.
        for q in queries.iter().filter(|q| q.kind == QueryKind::Blend) {
            let (w, i, j) =
                parse_blend_label(&q.text).unwrap_or_else(|| panic!("bad label {:?}", q.text));
            let mut expected = vec![0.0f32; DIMENSION];
            for (d, slot) in expected.iter_mut().enumerate() {
                *slot = (w as f32 * corpus[i][d]) + ((1.0 - w) as f32 * corpus[j][d]);
            }
            normalise(&mut expected);
            assert_eq!(expected, q.vector, "{}: label and vector disagree", q.id);
        }
    }

    /// Recovers `(w, i, j)` from a blend's provenance label.
    fn parse_blend_label(text: &str) -> Option<(f64, usize, usize)> {
        let (_, rest) = text.split_once("w=")?;
        let (w_str, rest) = rest.split_once(')')?;
        let w = w_str.parse().ok()?;
        let (_, rest) = rest.split_once("corpus[")?;
        let (i_str, rest) = rest.split_once(']')?;
        let (_, rest) = rest.split_once("corpus[")?;
        let (j_str, _) = rest.split_once(']')?;
        Some((w, i_str.parse().ok()?, j_str.parse().ok()?))
    }
}
