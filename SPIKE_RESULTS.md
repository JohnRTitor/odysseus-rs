# Spike Results — Phase 0 dependency verification

Every spike is run **in an isolated scratch directory** and its outcome recorded here
before any phase is designed around the dependency. A spike that fails becomes a
documented `REMOVED` in `INCOMPLETE.md`, not a silent fallback.

**Status: 2 of 11 resolved.** The rest are pending.

---

## S7 — `rusqlite` FTS5 + `sqlite-vec` — ✅ **PASS**

**This was the highest-priority gate.** Four downstream features (chat transcript
search, tool-selection RAG, personal-document RAG, semantic memory) are blocked
without it.

| Item | Finding |
|---|---|
| `rusqlite` | **0.40.2**, edition 2024, builds with `features = ["bundled"]` |
| `sqlite-vec` | **0.1.9**, builds sqlite-vec.c via `cc` |
| FTS5 | Works with `bundled`. **No extra feature flags needed.** |
| `PRAGMA foreign_keys` | Returns `1` as expected |
| Extension loading | `sqlite3_auto_extension(Some(transmute(sqlite_vec::sqlite3_vec_init)))` |
| MSRV | Not a constraint (verified on rustc 1.98.1) |

### The finding that matters most: default metric is L2

Test vectors chosen so cosine and L2 **provably disagree** — two vectors pointing the
same direction at different magnitudes:

| Table | Ordering | Distances |
|---|---|---|
| `vec0(... )` (default) | `x, y, z` | `x=0.10000, y=1.01980, z=1.41421` |
| `vec0(... distance_metric=cosine)` | `y, x, z` | `y=0.00496, x=0.00496, z=1.00000` |
| `vec0(... distance_metric=l2)` | `x, y, z` | identical to default |

**Conclusion: the default metric is L2, which is also ChromaDB's default.**

This materially de-risks the vector migration. If we had defaulted to cosine, every
threshold calibrated against ChromaDB (`>= 0.35` personal-doc similarity, the
`0.55/0.40/0.05` memory mix) would have needed full re-tuning from scratch. With L2 the
distance values are directly comparable and `F118` becomes a verification task rather
than a rediscovery exercise.

**Still required:** `DATA_MIGRATION.md` §5.3 golden-set validation. A shared default
metric is a strong prior, not proof.

### FTS5 trigger note

The Python schema's 3 triggers (`chat_messages_fts_ai/ad/au`) scrub base64 media from
the index. Those triggers reference `chat_messages`, so they cannot be tested in
isolation — `T0045` must create the base table first.

---

## S1/S2 — `ort` version and execution providers — ✅ **USABLE (CUDA unverified)**

| Item | Finding |
|---|---|
| Current version | **`2.0.0-rc.13`** — 20.5M downloads, last published 2026-07-28 |
| `ort = "2.0"` | **Does not resolve.** 2.0 stable was never published. Must pin `=2.0.0-rc.13`. |
| Builds on edition 2024 / rustc 1.98.1 | ✅ (type surface verified; MSRV is 1.88) |
| `api-22` marker | **Not a paid tier.** It is a version-gating feature, alongside `api-17`…`api-28`. |
| CUDA / TensorRT / CoreML / DirectML features | **Present in the open-source crate** (`cuda` → `ort-sys/cuda`, etc.) |
| EP type names | `ort::ep::CUDA`, `ort::ep::TensorRT` |

### Two corrections to the proposed template

```rust
// WRONG — these were the ort 1.x names
ort::execution_providers::TensorRTExecutionProvider::default().build()
ort::execution_providers::CUDAExecutionProvider::default().build()

// RIGHT — ort 2.0 names
ort::ep::TensorRT::default().build()
ort::ep::CUDA::default().build()
```

`with_execution_providers` takes `impl AsRef<[ExecutionProviderDispatch]>` in 2.0, and
each EP produces a dispatch via `.build()`.

### Third correction: timestep dtype

```rust
// WRONG for SD UNet exports — these expect int64, not f32
"timestep" => ndarray::arr0(timestep).view(),
```

### Verdict

**USABLE for API purposes. CUDA/TensorRT runtime is UNVERIFIED** — this machine has no
NVIDIA GPU, so enabling `cuda`/`tensorrt` would fail at build time (no `nvcc`).
Verdict must be re-confirmed on a CUDA machine before Phase 17 commits to it.

**Risk accepted:** `2.0.0-rc.13` is a release candidate. Mitigation is to isolate
`ort` inside `odysseus-ml` so a swap is a single-crate change.

---

## Corrections to assumptions in the planning documents

| Document | Was | Now |
|---|---|---|
| `REWRITE_PLAN.md` D10 | "pin `2.0.0-rc.13`" | ✅ confirmed correct |
| `INCOMPLETE.md` S1 | "EPs may be gated behind `api-22`" | ✅ resolved — not gated |
| `INCOMPLETE.md` S7 | "FTS5 + sqlite-vec must be confirmed" | ✅ resolved — both work |
| `DATA_MIGRATION.md` §5.2 | Thresholds need full re-tuning | **Downgraded** — L2 matches ChromaDB, so this is verification not rediscovery |

---

## Pending spikes

| Spike | Question | Blocks |
|---|---|---|
| **S3** | SAM ONNX: upstream `export_onnx_model.py` exports only prompt encoder + mask decoder, not the ViT image encoder. Need a separate `image_encoder.onnx`. | F168 |
| **S4** | Does `any-tts` (17,967 dl, 5 versions, updated 2026-08) provide a Kokoro backend? Would make `kokoroxide` (1,734 dl, WIP, 7 stars) moot | F338 |
| **S5** | `rembg-rs` (817 dl, 14 commits) — is it dependable, or should U2Net be run directly via `ort`? | F169 |
| **S6** | `whisper-rs` (1.4M dl) — CUDA support, and is whisper.cpp a real regression vs faster-whisper? | F339 |
| **S8** | `lettre` (19.4M dl) / `async-imap` (1.7M dl) / `mailparse` (14.6M dl) — API quality for a 6,152-line subsystem | Phase 15 |
| **S9** | Local Stable Diffusion scope — is it needed given inpaint/harmonize already proxy? | F176 |
| **S10** | `lopdf` (22.3M dl) for AcroForm; `mupdf`/`poppler-rs` for rasterisation. **C-dependency implications** | F146, F148 |
| **S11** | Capture the sqlite-vec golden set **while ChromaDB is still running** | F118 |

### S11 is time-critical

The golden set must be captured from the live ChromaDB instance *before* anything
touches vectors. Once that service is stopped or its data directory removed, the
calibration baseline is unrecoverable. Schedule it before Phase 7 begins.

---

## Crate maturity snapshot

Download counts from the crates.io API, 2026-10-03. Low download count relative to age
is a maintenance risk signal.

| Crate | Version | Downloads | Last publish | Verdict |
|---|---|---|---|---|
| `ort` | 2.0.0-rc.13 | 20,501,776 | 2026-07-28 | Mature, but an RC |
| `rusqlite` | 0.40.2 | 116,748,060 | 2026-08-08 | Mature |
| `sqlite-vec` | 0.1.9 | 3,158,097 | 2026-05-18 | Healthy |
| `lopdf` | 0.45.0 | 22,265,169 | 2026-09-08 | Mature |
| `lettre` | 0.11.23 | 19,426,758 | 2026-08-03 | Mature |
| `mailparse` | 0.17.0 | 14,596,583 | 2026-09-06 | Mature |
| `async-imap` | 0.12.0 | 1,748,405 | 2026-09-30 | Actively maintained |
| `mail-builder` | 1.0.0 | 1,432,641 | 2026-09-12 | Healthy |
| `whisper-rs` | 0.16.0 | 1,406,056 | 2026-03-12 | Healthy |
| `mupdf` | 0.8.0 | 1,709,128 | 2026-06-22 | Healthy |
| `poppler-rs` | 0.26.0 | 246,144 | 2026-02-21 | **Low adoption** |
| `any-tts` | 0.1.3 | 17,967 | 2026-08-26 | **Early** |
| `kokoroxide` | 0.1.5 | 1,734 | 2025-10-04 | **Stale, marked WIP** |
| `rembg-rs` | 0.1.4 | 817 | 2025-12-28 | **Effectively unmaintained** |