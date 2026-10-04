//! Captures the ChromaDB vector golden set from a live service.
//!
//! Run it while ChromaDB is still up. See `README.md` in this directory for
//! why this cannot be re-done later.
//!
//! What it records, per query, per collection:
//!   * the raw `distance` Chroma returned, and
//!   * `similarity = 1.0 - distance`, the convention the Python app uses.
//!
//! Both are stored on purpose. Storing only the derived similarity makes it
//! impossible to tell, years later, whether a regression broke the metric or
//! only the conversion.
//!
//! Collections that are empty at capture time are recorded as empty, with their
//! record count. A fixture that silently omits them is worse than one that says
//! "this was empty, and here is the count".

use std::{collections::BTreeMap, error::Error, fs, path::PathBuf};

use {
    capture_golden_set::{
        DIMENSION, QUERY_COUNT, Query, SEED, TOP_K, Vector, build_queries, cosine_distance,
    },
    serde::{Deserialize, Serialize},
    serde_json::{Value, json},
};

/// Fixture format version. Bump on any incompatible change to the layout.
const SCHEMA_VERSION: u32 = 1;

/// The one collection expected to carry data. Also the collection the query
/// vectors are built from.
const SEED_COLLECTION: &str = "odysseus_tool_index_fastembed";

fn main() -> Result<(), Box<dyn Error>> {
    let (chroma, out) = parse_args();
    let base = format!("{}/api/v2", chroma.trim_end_matches('/'));
    let api = format!("{base}/tenants/default_tenant/databases/default_database/collections");
    eprintln!("chroma: {api}");

    let version: String = get(&format!("{base}/version"))?;
    eprintln!("chroma version: {version}");

    let listed: Vec<ListedCollection> = get(&api)?;
    if listed.is_empty() {
        return Err("no collections found; is this the right ChromaDB?".into());
    }

    // ---- pass 1: read every collection's records and vector metadata ----
    let mut collections = Vec::new();
    let mut corpora: BTreeMap<String, Vec<CorpusRow>> = BTreeMap::new();
    for entry in &listed {
        let page: GetResponse = post(
            &format!("{api}/{}/get", entry.id),
            &json!({ "include": ["documents", "metadatas", "embeddings"], "limit": 10_000 }),
        )?;
        let rows = page.into_rows()?;
        eprintln!("collection {:<32} records={}", entry.name, rows.len());
        collections.push(CollectionRecord {
            name: entry.name.clone(),
            id: entry.id.clone(),
            record_count: rows.len(),
            empty_at_capture: rows.is_empty(),
            hnsw_space: entry.space(),
            embedding_dimension: entry.metadata_int("embedding_dimension"),
            embedding_fingerprint: entry.metadata_str("embedding_fingerprint"),
            embedding_model: entry.metadata_str("embedding_model"),
            embedding_lane: entry.metadata_str("embedding_lane"),
        });
        corpora.insert(entry.name.clone(), rows);
    }

    // ---- build the one query set, from the richest collection ----
    let seed_corpus = corpora
        .get(SEED_COLLECTION)
        .ok_or_else(|| format!("{SEED_COLLECTION} not present"))?;
    if seed_corpus.len() < 2 {
        return Err(format!(
            "{SEED_COLLECTION} has {} records; need >= 2",
            seed_corpus.len()
        )
        .into());
    }
    let seed_vectors = embeddings_of(seed_corpus)?;
    let queries = build_queries(&seed_vectors);
    eprintln!(
        "built {} deterministic queries from {SEED_COLLECTION}",
        queries.len()
    );

    // ---- pass 2: ask Chroma for the ground truth, once per collection ----
    let mut hits: BTreeMap<String, Vec<QueryHits>> = BTreeMap::new();
    for entry in &listed {
        let empty = collections
            .iter()
            .find(|c| c.name == entry.name)
            .is_some_and(|c| c.empty_at_capture);
        if empty {
            eprintln!("querying {:<32} skipped (empty at capture)", entry.name);
            hits.insert(entry.name.clone(), Vec::new());
            continue;
        }
        let response: QueryResponse = post(
            &format!("{api}/{}/query", entry.id),
            &json!({
                "query_embeddings": queries.iter().map(|q| &q.vector).collect::<Vec<_>>(),
                "n_results": TOP_K,
                "include": ["distances", "metadatas", "documents"],
            }),
        )?;
        let rows = response.into_hits()?;
        if rows.len() != QUERY_COUNT {
            return Err(format!(
                "{}: expected {QUERY_COUNT} query rows, got {}",
                entry.name,
                rows.len()
            )
            .into());
        }
        eprintln!("querying {:<32} {QUERY_COUNT} x top-{TOP_K}", entry.name);
        hits.insert(entry.name.clone(), rows);
    }

    // ---- closed-form cross-check against the corpus we hold ----
    let noise = noise_floor(&queries, &hits, seed_corpus)?;

    let records = queries
        .into_iter()
        .enumerate()
        .map(|(qi, q)| QueryRecord {
            id: q.id,
            kind: q.kind,
            text: q.text,
            vector: Vector::new(q.vector),
            results: collections
                .iter()
                .map(|c| {
                    // An empty collection has no per-query rows at all, which is
                    // the honest representation: zero results, recorded as
                    // empty, with the count kept on the collection record.
                    let rows = if c.empty_at_capture {
                        Vec::new()
                    } else {
                        hits.get(&c.name)
                            .map_or_else(Vec::new, |all| all[qi].hits.clone())
                    };
                    QueryCollectionResult {
                        collection: c.name.clone(),
                        status: if c.empty_at_capture {
                            HitStatus::EmptyCollectionAtCapture
                        } else {
                            HitStatus::Ok
                        },
                        hits: rows,
                    }
                })
                .collect(),
        })
        .collect();

    let fixture = Fixture {
        schema_version: SCHEMA_VERSION,
        captured_at_utc: now_utc(),
        generator: Generator {
            tool: "tools/capture-golden-set",
            query_construction:
                "identity queries (one per corpus document, verbatim), then blends \
                                 w*corpus[i] + (1-w)*corpus[j] renormalised, w cycling 0.1..=0.9"
                    .to_string(),
            prng: "SplitMix64",
            seed: format!("0x{SEED:016X}"),
            seed_collection: SEED_COLLECTION.to_string(),
            query_count: QUERY_COUNT,
            top_k: TOP_K,
        },
        source: Source {
            chroma_version: version,
            api: api.clone(),
            note: "Capture-time ground truth. Once the ChromaDB service or its data directory is \
                   gone this cannot be recaptured."
                .to_string(),
        },
        metric: Metric {
            declared: "cosine",
            set_by: "explicit client metadata `hnsw:space: cosine` (src/embedding_lanes.py:81); \
                     confirmed live as configuration_json.hnsw.space"
                .to_string(),
            distance_convention: "cosine_distance = 1 - cosine_similarity (src/memory_vector.py:136-137)",
            similarity_convention: "similarity = 1.0 - distance",
            both_recorded: true,
            vectors_normalised_on_insert: true,
            noise_floor: noise,
        },
        embedding: Embedding {
            model: collections
                .first()
                .and_then(|c| c.embedding_model.clone())
                .unwrap_or_default(),
            dimension: DIMENSION,
            fingerprint: collections
                .first()
                .and_then(|c| c.embedding_fingerprint.clone())
                .unwrap_or_default(),
            lane: collections
                .first()
                .and_then(|c| c.embedding_lane.clone())
                .unwrap_or_default(),
        },
        collections,
        corpus: seed_corpus
            .iter()
            .map(|r| CorpusRecord {
                id: r.id.clone(),
                document: r.document.clone(),
                metadata: r.metadata.clone(),
                vector: Vector::new(r.embedding.clone().unwrap_or_default()),
            })
            .collect(),
        queries: records,
    };

    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&out, serde_json::to_string_pretty(&fixture)?)?;
    eprintln!(
        "wrote {} ({:.2} MiB, {} collections, {} queries, {} corpus rows)",
        out.display(),
        fs::metadata(&out)?.len() as f64 / 1_048_576.0,
        fixture.collections.len(),
        fixture.queries.len(),
        fixture.corpus.len()
    );
    Ok(())
}

/// Recomputes every recorded distance from the corpus embeddings we already hold
/// and reports the spread. This is the achievable agreement floor for any replay:
/// Chroma computes in f32, this recomputes in f64, so the two can never agree
/// exactly. Recording the gap is what stops a future port from chasing it.
fn noise_floor(
    queries: &[Query],
    hits: &BTreeMap<String, Vec<QueryHits>>,
    corpus: &[CorpusRow],
) -> Result<NoiseFloor, Box<dyn Error>> {
    let by_id: BTreeMap<&str, &[f32]> = corpus
        .iter()
        .filter_map(|r| r.embedding.as_deref().map(|v| (r.id.as_str(), v)))
        .collect();

    let mut deltas = Vec::new();
    for (qi, q) in queries.iter().enumerate() {
        let Some(row) = hits.get(SEED_COLLECTION).and_then(|r| r.get(qi)) else {
            continue;
        };
        for hit in &row.hits {
            let Some(vector) = by_id.get(hit.id.as_str()) else {
                return Err(format!("hit {} is not in the captured corpus", hit.id).into());
            };
            deltas.push((cosine_distance(&q.vector, vector) - hit.distance).abs());
        }
    }
    if deltas.is_empty() {
        return Err("no distances to cross-check".into());
    }
    deltas.sort_by(f64::total_cmp);
    let pick = |frac: f64| deltas[((deltas.len() as f64 * frac) as usize).min(deltas.len() - 1)];
    Ok(NoiseFloor {
        samples: deltas.len(),
        median_abs: pick(0.5),
        p99_abs: pick(0.99),
        max_abs: *deltas.last().unwrap_or(&0.0),
    })
}

fn embeddings_of(rows: &[CorpusRow]) -> Result<Vec<Vec<f32>>, Box<dyn Error>> {
    rows.iter()
        .map(|r| {
            r.embedding
                .clone()
                .ok_or_else(|| format!("{} has no stored embedding", r.id).into())
        })
        .collect()
}

fn parse_args() -> (String, PathBuf) {
    let mut chroma = "http://127.0.0.1:8100".to_string();
    let mut out = PathBuf::from("docs/baseline/vector_golden_set.json");
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--chroma" => chroma = args.next().unwrap_or_else(|| chroma.clone()),
            "--out" => {
                out = PathBuf::from(args.next().unwrap_or_else(|| out.display().to_string()))
            }
            other => eprintln!("ignoring unknown argument {other}"),
        }
    }
    (chroma, out)
}

// ---------------------------------------------------------------- HTTP

fn get<T: for<'de> Deserialize<'de>>(url: &str) -> Result<T, Box<dyn Error>> {
    Ok(ureq::get(url).call()?.body_mut().read_json()?)
}

fn post<T: for<'de> Deserialize<'de>>(url: &str, body: &Value) -> Result<T, Box<dyn Error>> {
    Ok(ureq::post(url).send_json(body)?.body_mut().read_json()?)
}

/// UTC timestamp to the second, from the system clock. The only
/// non-deterministic field in the fixture, and it is metadata: every number the
/// tests assert on is reproducible.
fn now_utc() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    let days = i64::try_from(secs / 86_400).unwrap_or_default();
    let tod = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    let (hh, mm, ss) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// Howard Hinnant's civil-from-days, so the capture tool needs no date crate.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = u32::try_from(doy - (153 * mp + 2) / 5 + 1).unwrap_or(1);
    let m = u32::try_from(if mp < 10 { mp + 3 } else { mp - 9 }).unwrap_or(1);
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// ---------------------------------------------------------------- wire types

#[derive(Debug, Deserialize)]
struct ListedCollection {
    name: String,
    id: String,
    #[serde(default)]
    configuration_json: Option<ConfigurationJson>,
    #[serde(default)]
    metadata: Option<BTreeMap<String, Value>>,
}

#[derive(Debug, Deserialize)]
struct ConfigurationJson {
    #[serde(default)]
    hnsw: Option<Value>,
}

impl ListedCollection {
    fn space(&self) -> Option<String> {
        self.configuration_json
            .as_ref()?
            .hnsw
            .as_ref()?
            .get("space")?
            .as_str()
            .map(str::to_string)
    }

    fn metadata_str(&self, key: &str) -> Option<String> {
        self.metadata
            .as_ref()?
            .get(key)?
            .as_str()
            .map(str::to_string)
    }

    fn metadata_int(&self, key: &str) -> Option<i64> {
        self.metadata.as_ref()?.get(key)?.as_i64()
    }
}

#[derive(Debug, Deserialize)]
struct GetResponse {
    #[serde(default)]
    ids: Vec<String>,
    #[serde(default)]
    documents: Option<Vec<Option<String>>>,
    #[serde(default)]
    metadatas: Option<Vec<Option<BTreeMap<String, Value>>>>,
    #[serde(default)]
    embeddings: Option<Vec<Option<Vec<f32>>>>,
}

impl GetResponse {
    /// Chroma returns these as parallel arrays. A ragged response means ids and
    /// embeddings have drifted apart, which would silently mis-pair every
    /// vector with the wrong document, so it is an error rather than a zip.
    fn into_rows(self) -> Result<Vec<CorpusRow>, Box<dyn Error>> {
        let n = self.ids.len();
        for (label, len) in [
            ("documents", self.documents.as_ref().map(Vec::len)),
            ("metadatas", self.metadatas.as_ref().map(Vec::len)),
            ("embeddings", self.embeddings.as_ref().map(Vec::len)),
        ] {
            if let Some(len) = len
                && len != n
            {
                return Err(format!("ragged get response: ids={n} {label}={len}").into());
            }
        }
        let at = |i: usize| -> CorpusRow {
            CorpusRow {
                id: self.ids[i].clone(),
                document: self
                    .documents
                    .as_ref()
                    .and_then(|d| d.get(i))
                    .cloned()
                    .flatten(),
                metadata: self
                    .metadatas
                    .as_ref()
                    .and_then(|m| m.get(i))
                    .cloned()
                    .flatten()
                    .unwrap_or_default(),
                embedding: self
                    .embeddings
                    .as_ref()
                    .and_then(|e| e.get(i))
                    .cloned()
                    .flatten(),
            }
        };
        Ok((0..n).map(at).collect())
    }
}

#[derive(Debug, Clone, Deserialize)]
struct CorpusRow {
    id: String,
    document: Option<String>,
    metadata: BTreeMap<String, Value>,
    embedding: Option<Vec<f32>>,
}

#[derive(Debug, Deserialize)]
struct QueryResponse {
    #[serde(default)]
    ids: Vec<Vec<String>>,
    #[serde(default)]
    distances: Vec<Vec<Option<f64>>>,
}

impl QueryResponse {
    fn into_hits(self) -> Result<Vec<QueryHits>, Box<dyn Error>> {
        if self.ids.len() != self.distances.len() {
            return Err("ragged query response: ids vs distances".into());
        }
        self.ids
            .iter()
            .enumerate()
            .map(|(qi, ids)| {
                let dists = &self.distances[qi];
                if ids.len() != dists.len() {
                    return Err(format!(
                        "query {qi}: {} ids vs {} distances",
                        ids.len(),
                        dists.len()
                    )
                    .into());
                }
                let hits = ids
                    .iter()
                    .enumerate()
                    .map(|(hi, id)| {
                        let distance = dists[hi]
                            .ok_or_else(|| format!("query {qi} hit {hi}: null distance"))?;
                        Ok(Hit {
                            rank: hi + 1,
                            id: id.clone(),
                            distance,
                            similarity: 1.0 - distance,
                        })
                    })
                    .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
                Ok(QueryHits { hits })
            })
            .collect()
    }
}

// ---------------------------------------------------------------- fixture

#[derive(Debug, Serialize)]
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

#[derive(Debug, Serialize)]
struct Generator {
    tool: &'static str,
    query_construction: String,
    prng: &'static str,
    seed: String,
    seed_collection: String,
    query_count: usize,
    top_k: usize,
}

#[derive(Debug, Serialize)]
struct Source {
    chroma_version: String,
    api: String,
    note: String,
}

#[derive(Debug, Serialize)]
struct NoiseFloor {
    samples: usize,
    median_abs: f64,
    p99_abs: f64,
    max_abs: f64,
}

#[derive(Debug, Serialize)]
struct Metric {
    declared: &'static str,
    set_by: String,
    distance_convention: &'static str,
    similarity_convention: &'static str,
    both_recorded: bool,
    vectors_normalised_on_insert: bool,
    noise_floor: NoiseFloor,
}

#[derive(Debug, Serialize)]
struct Embedding {
    model: String,
    dimension: usize,
    fingerprint: String,
    lane: String,
}

#[derive(Debug, Serialize)]
struct CollectionRecord {
    name: String,
    id: String,
    record_count: usize,
    empty_at_capture: bool,
    hnsw_space: Option<String>,
    embedding_dimension: Option<i64>,
    embedding_fingerprint: Option<String>,
    embedding_model: Option<String>,
    embedding_lane: Option<String>,
}

#[derive(Debug, Serialize)]
struct CorpusRecord {
    id: String,
    document: Option<String>,
    metadata: BTreeMap<String, Value>,
    vector: Vector,
}

#[derive(Debug, Serialize)]
struct QueryRecord {
    id: String,
    kind: capture_golden_set::QueryKind,
    text: String,
    vector: Vector,
    results: Vec<QueryCollectionResult>,
}

#[derive(Debug, Clone, Serialize)]
struct QueryCollectionResult {
    collection: String,
    status: HitStatus,
    hits: Vec<Hit>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum HitStatus {
    Ok,
    EmptyCollectionAtCapture,
}

#[derive(Debug, Clone, Serialize)]
struct QueryHits {
    hits: Vec<Hit>,
}

#[derive(Debug, Clone, Serialize)]
struct Hit {
    rank: usize,
    id: String,
    distance: f64,
    similarity: f64,
}
