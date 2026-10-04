# `capture-golden-set` — the ChromaDB vector golden set

Task **T0030** / spike **S11**. Produces `docs/baseline/vector_golden_set.json`.

## Read this before touching anything

**This is the only copy of the calibration baseline, and it cannot be
regenerated.** The ChromaDB service at `127.0.0.1:8100` and its data directory
are the only place the pre-migration vector state exists. Once the service is
stopped or `data/chroma/` is removed, every threshold recorded against
F118/F124 — `similarity >= 0.35`, `vs < 0.20`, `final > 0.12`,
`find_similar(threshold=0.92)`, the `0.55/0.40/0.05` memory mix, the
`0.7/0.3` rag mix — loses its calibration and must be re-derived from scratch.
That is why `DATA_MIGRATION.md` §5.3 gates the vector port on this capture.

The capture has already been taken (2026-10-04). **Do not delete
`docs/baseline/vector_golden_set.json` to "regenerate" it** — regenerating
requires the service, and by then it will be gone.

## Running it

```sh
cargo run --manifest-path tools/capture-golden-set/Cargo.toml -- \
    --chroma http://127.0.0.1:8100 \
    --out docs/baseline/vector_golden_set.json
```

Not a workspace member, for the same reason as `tools/dump-baseline`: this tool's
entire value is that it runs once, by hand, against a service that will not exist
in CI. It must never enter the shipped build graph.

Requires a live ChromaDB ≥ 1.0 on **API v2**, which needs both a tenant and a
database segment. `/api/v1/*` is deprecated and the collections path without the
`databases` segment 404s.

## What it records

Per query, per collection: **both** the raw `distance` Chroma returned *and*
`similarity = 1.0 - distance`, the convention the Python app inverts at
`src/memory_vector.py:136-137`. Storing only the derived similarity would make it
impossible to tell, years later, whether a regression broke the metric or only
the conversion — which is the whole point of recording both.

Also recorded, because a golden set without them is not reproducible:

| Field | Why |
|---|---|
| `metric.declared` = `cosine` | **Not** Chroma's default. It is set explicitly by the client at `src/embedding_lanes.py:81` (`"hnsw:space": "cosine"`), confirmed live as `configuration_json.hnsw.space`. `SPIKE_RESULTS.md` S7's claim that sqlite-vec's L2 default "matches ChromaDB's default" is wrong — see below. |
| `metric.noise_floor` | Chroma computes in `f32`; the cross-check recomputes in `f64`. The gap between them is the best agreement any replay can achieve. Recorded so nobody chases a discrepancy smaller than it. |
| `embedding.{model,dimension,fingerprint,lane}` | If the lane is ever re-embedded, the fingerprint changes and this fixture stops describing the live data. Asserted by the tests. |
| `collections[].empty_at_capture` | Two of the three collections were **empty**. Recorded as empty with their record count, not omitted — a fixture that silently dropped them would be indistinguishable from one where they had data. |
| `generator.{prng,seed,seed_collection}` | The queries are derived, not hand-written. Without the seed, nobody could rebuild them. |
| `queries[].vector`, `corpus[].vector` | Raw `f32`, full precision. **Vectors, not just query text**, so replaying the set never needs the embedding endpoint — a golden set that depends on a live service is the opposite of what a golden set is for. |

## Why the query vectors come from the corpus

A query vector must live in the *same* space as the corpus embeddings for its
distances to mean anything. The stored corpus embeddings are the only vectors
whose space is provably correct, so the 200 queries are derived from them:

- **`q000`–`q068` — identity.** Each corpus document's own embedding, verbatim.
  These are the set's internal consistency anchor: a self-match must come back at
  distance ≈ 0 under cosine, and at ≈ 0.38 under L2 on the same normalised
  vectors. That is what pins the metric in
  `recorded_distances_agree_with_closed_form_cosine`.
- **`q069`–`q199` — blends.** `w·corpus[i] + (1−w)·corpus[j]`, renormalised,
  with `w` cycling `0.1..=0.9`. A real query vector is a blend of concepts, and
  sweeping `w` spreads the expected distances across the range instead of
  clustering near one value — which is what lets the set discriminate between a
  correct port and a broken one.

`i`, `j` and `w` come from a **SplitMix64** stream seeded with a constant, chosen
over `rand` because SplitMix64 is fully specified and identical on every platform
and Rust version. `query_vectors_are_reproducible_from_the_seed_and_the_corpus`
rebuilds the whole set from the fixture's own corpus and requires bit-identical
vectors.

## What it deliberately does not do

No sqlite-vec schema, no retrieval code, no `odysseus-store` / `odysseus-ml`
source. This task produced a fixture and the script that made it. The port reads
the fixture; it does not live here.

## Replaying it (for whoever does the port)

The fixture is self-verifying: `recorded_distances_agree_with_closed_form_cosine`
re-derives all 2,000 recorded distances from the fixture's own corpus embeddings
and requires agreement within `metric.noise_floor.max_abs`. So a port can be
checked two ways — against the recording *and* against cosine geometry — and a
disagreement larger than the noise floor is a real defect, not noise.

One caveat found while building this: **`serde_json`'s default float parser is
not correctly rounded.** It can read a 17-significant-digit value back one ULP
away, which is why `similarity_is_one_minus_distance_on_every_row` allows 1 ULP
rather than demanding bit equality. The generator enables
`serde_json/float_roundtrip` so the file it writes does round-trip exactly under
its own parser; a consumer using a different parser may still see the last bit
move. That is ~1e-16, nine orders of magnitude below the noise floor.