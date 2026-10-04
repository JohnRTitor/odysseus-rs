# Spike Results — Phase 0 dependency verification

Every spike is run **in an isolated scratch directory** and its outcome recorded here
before any phase is designed around the dependency. A spike that fails becomes a
documented `REMOVED` in `INCOMPLETE.md`, not a silent fallback.

**Status: 11 of 11 resolved.** S3, S4, S6, S8, S9 and S10 were resolved on 2026-10-04; every section above carries its evidence and its limits. No spike is unresolved and none was closed without a run or a source citation.

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

**Conclusion: the default metric is L2.**

> **CORRECTION (2026-10-03).** This section originally concluded *"the default metric
> is L2, which is also ChromaDB's default"*, and everything downstream rested on the
> second clause. **That clause was false, and the conclusions drawn from it were
> wrong.** ChromaDB's default is irrelevant here anyway: the Python client sets the
> metric **explicitly, per lane** — `src/embedding_lanes.py:80` hard-codes
> `"hnsw:space": "cosine"` inside `_metadata()`. Live collections confirm it.
>
> The finding above is still correct that sqlite-vec defaults to L2, and the table
> above is still correct that cosine and L2 **provably disagree**. But the direction
> of the risk was inverted. Because Python uses cosine, the port **must** declare
> `vec0(..., distance_metric=cosine)`; defaulting to L2 is the defect, not the
> safe choice.
>
> Why it matters: every threshold in the system is calibrated on `1.0 - cosine_distance`.
> Under L2, `1.0 - distance` is not a similarity at all — it is unbounded, and it moves
> the *wrong way* as vectors diverge. See the corrected analysis below.

**All consumers calibrate on `1.0 - cosine_distance`.** Verified in the Python source:

| Consumer | Line | Code |
|---|---|---|
| `rag_vector.py` | 379, 383 | `vector_sim = 1.0 - distance`; `hybrid_score = 0.7*vector_sim + 0.3*keyword_score` |
| `memory_vector.py` | 137, 157, 181 | comment *"ChromaDB cosine distance = 1 - cosine_similarity. We convert back"*; `"score": round(1.0 - distance, 4)` |
| `memory_vector.py` | 165 | `find_similar(threshold=0.92)` |
| `tool_index.py` | 324 | `"score": round(1.0 - distance, 4)` |
| `chat_processor.py` | 91 | `RAG_SIMILARITY_THRESHOLD = 0.35` |

Vectors are L2-normalised at insert (`embedding_lanes.py:132`,
`normalize_embeddings=True`), so for unit vectors Chroma's cosine distance is exactly
`1 - cos_sim` and `1.0 - distance` lands in `[0, 1]` — which is the scale every one of
`0.92`, `0.35`, `0.20`, `0.12`, and the `0.55/0.40/0.05` and `0.7/0.3` mixes is written
against.

**Therefore the port must:** declare `vec0(..., distance_metric=cosine)` explicitly
(the table above proves sqlite-vec supports it), keep `1.0 - distance → similarity`,
keep `normalize_embeddings = true`, and reproduce the lane split as one sqlite-vec
table per `{base, lane}` pair keyed by the embedding fingerprint
(`sha256(lane, url, model, dimension)[:16]`). Then `DATA_MIGRATION.md` §5.3's golden
set is genuine verification.

*Rejected alternative:* normalise and then rely on L2-on-unit-vectors, where
`‖a−b‖² = 2 − 2·cos` makes the metrics arithmetically equivalent. It works, but it is
a trick that invites a future bug; naming the metric explicitly is cheaper.

**Still required:** `DATA_MIGRATION.md` §5.3 golden-set validation — now genuinely
load-bearing rather than a formality.

**Golden-set precondition (was wrong, now resolved):** an earlier note here recorded
the live collections as empty. They are not — there are **three** collections
(`collection_name(base, lane) = f"{base}_{lane}"`, `embedding_lanes.py:69-70`), all
cosine, all dim 384, all with fingerprint `9d32c541ea92982d`, and the tool-index lane
holds **69 records** (`builtin_bash`, `builtin_python`, …). Only `rag` and `memories`
are empty. Planning around "0 records" would have discarded the only lane with data.

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

## S5 — `rembg-rs` U2Net alpha matte — ⚠️ **FAIL as published / capability confirmed**

**Verdict: FAIL.** `rembg-rs 0.1.4` does not compile. It worked when it was
published; it rotted when `ort` moved on.

| Item | Finding |
|---|---|
| Crate version | **0.1.4**, published **2025-12-28** — unmoved since. 818 downloads, 9 stars, 2 open issues, **no push since the release commit** |
| Build as published | ❌ **4 compile errors.** `ort = "2.0.0-rc.10"` is a caret req that resolves to **rc.13** |
| Build with `ort = "=2.0.0-rc.10"` | ✅ Compiles, and U2Net inference works |
| `ort` resolved | **2.0.0-rc.13**, `ndarray` 0.17.2 (single version, no conflict) |
| Weights | `u2net.onnx`, **175,997,641 B**, from `danielgatis/rembg` release `v0.0.0`. Loads in ~0.5–0.9 s |
| Inference | **634 ms median** for 480×360, **CPU only, no GPU** |
| Alpha matte | ✅ real: min 0, max 255, mean 76.3; 29.9 % opaque / 70.1 % clear; fg fraction 0.299 vs geometric 0.299 |
| Soft edges | ❌ **0 partial-alpha pixels in every mode.** See defect below |
| Runtime acquisition | `ort` **`load-dynamic`** → needs a system `libonnxruntime.so`. The README's "installed automatically" is false |
| Runtime version coupling | `ort-sys` rc.13 wants ORT **1.28**, rc.10 wants **1.22**, nixpkgs ships **1.27.1**. The crate's own script installs **1.16.0** — too old for both |
| Transitive deps | 240 packages, incl. `imageproc`, `imagequant`, `oxipng`, `png` |
| Licences | crate **MIT**; `ort` MIT/Apache-2.0; weights **MIT** (rembg) / **Apache-2.0** (U-2-Net). All already allowed by `deny.toml` — but `deny.toml` has no clause for redistributed model weights |

### Why it fails: dependency rot, not a host problem

`ort 2.0.0-rc.10` shipped 2025-06-01, `rc.11` on 2026-01-07, `rc.13` on 2026-07-28.
`rembg-rs 0.1.4` shipped 2025-12-28, when `^2.0.0-rc.10` still resolved to rc.10.
`ort` then made `Error` generic over its build stage and turned `Session::inputs` /
`outputs` from fields into methods, which broke four call sites in
`rembg-rs/src/manager.rs`. Pinning `ort = "=2.0.0-rc.10"` restores the build and
U2Net then runs correctly — so the **capability is real and the packaging is broken**.
A control crate with bare `ort = "=2.0.0-rc.13"` reproduces the failure identically,
proving the breakage is `rembg-rs`'s rather than the host's.

### Defect: `binary: false` never produces semi-transparency

`RemovalOptions::binary` is documented as *"If false, allows soft edges for more
natural blending"*. `src/rembg.rs:120-148` computes

    alpha = ((mask - threshold) * (255 / (255 - threshold)) * 255).clamp(0, 255)

The scale is applied twice, so any mask value `>= threshold + 1` saturates to 255.
Measured on the same image: partial-alpha pixel count is **0** for
`(binary=false, thr=160)`, `(binary=true, thr=160)` and `(binary=false, thr=128)`.
The output is a **hard binary cutout** — aliased edges on hair, fur and
semi-transparent subjects, which is the main reason to prefer U2Net over a
threshold. The raw sigmoid mask is never exposed; `RemovalResult::mask` is a
colourised heatmap, not a usable alpha channel.

### Recommendation: do not ship it, even though the capability works

1. **It cannot be depended on as published.** Any `cargo update` that moves `ort` off
   rc.10 breaks the build. `ort` is an RC that has shipped four versions in fourteen
   months — this is not hypothetical, it already happened once.
2. **Supporting it means pinning the whole workspace's `ort`** to a June-2025 RC, to
   accommodate a 9-star library that stopped committing in December. That downgrades
   F176's and F168's runtime too. A far worse trade than writing the ~150 lines.
3. **It does not deliver the feature.** A binary cutout is what a luminance threshold
   gives for free. The thing worth having — soft alpha — is unreachable via its API.
4. **240 packages** including `oxipng`, `imagequant` and `imageproc` for one inference
   call. Against AGENTS.md §12 ("check the stdlib or an existing dependency first"),
   `ort` is already the dependency.

### Consequence

F169 is **REMOVED**. If the capability is ever wanted back, run U2Net **directly via
`ort`** — `ort` is already the workspace's ML runtime and the graph is a single
320×320 NCHW input → 1×1×320×320 logit output. That is ~150 lines and gives a correct
soft alpha; `rembg-rs` adds 240 packages to save them and returns a binary mask anyway.

**Policy gap found:** `deny.toml` covers crates only; it has no clause for
redistributed **model weights**. Not a blocker here, but any future spike that vendors
weights needs one.


## S8 — `lettre` / `async-imap` / `mailparse` — ✅ **PASS (XOAUTH2 verified on the wire)**

**Phase 15 does not need re-planning.** Gmail OAuth2 works on both halves, and the
XOAUTH2 SASL string — the single most error-prone part — was checked byte-for-byte
against Google's published format by running a real SMTP dialogue.

```
lettre test_connection -> Ok(true)
wire:  AUTH XOAUTH2 dXNlcj1hbGljZUBleGFtcGxlLmNvbQFhdXRoPUJlYXJlciB5YTI5LmEwQWZINlNNQngtYWNjZXNzLXRva2VuAQE=
bytes: user=alice@example.com\u{1}auth=Bearer ya29.a0AfH6SMBx-access-token\u{1}\u{1}
```

Google publishes that exact layout (`user=` + `\x01` + `auth=Bearer ` + token + `\x01\x01`).

| Crate | Version | Licence | Evidence |
|---|---|---|---|
| `lettre` | 0.11.23 | MIT | `transport/smtp/authentication.rs:117-123` builds the XOAUTH2 string; verified on the wire above |
| `async-imap` | 0.12.0 | MIT OR Apache-2.0 | `client.rs:244-297` `authenticate("XOAUTH2", &auth)` with a custom `Authenticator` (`type Response: AsRef<[u8]>`) |
| `mailparse` | 0.17.0 | 0BSD | multipart/alternative parsed: 2 parts, subject recovered |

Transitive deps: **132** for all three.

### Three gotchas worth writing down

1. **XOAUTH2 is not in lettre's defaults.** `DEFAULT_MECHANISMS = [Plain, Login]`
   (`authentication.rs:10`). You must pass
   `.authentication(vec![Mechanism::Xoauth2])` explicitly, with the bearer token as
   `Credentials::new(user, token)`. Fail-safe in practice — Gmail rejects PLAIN — but
   the failure will read as "auth failed", not "you forgot the mechanism".
2. **`.tls()` is feature-gated.** It only exists under a TLS backend feature
   (`tokio1-native-tls` / `tokio1-rustls` / …). With `default-features = false` the
   builder has no TLS method at all, which fails at compile time rather than silently
   sending mail in the clear. Use `Tls::Required` on port 587 (STARTTLS, MITM-safe) or
   `Tls::Wrapper` on 465 (implicit).
3. **Gmail IMAP is implicit TLS on 993, not STARTTLS.** `async-imap` supports it via
   `async_imap::connect((domain, 993), domain, tls)` with an
   `async_native_tls::TlsConnector`. Gmail advertises `LOGINDISABLED`, so XOAUTH2 is the
   only path — which is what you want.

The async-imap `Authenticator` shape is also the right place for **token refresh
mid-handshake**, which a fixed lettre `Credentials` cannot do. Note that as a real
design constraint for Phase 15: the SMTP bearer token must be re-read per send.

**Verdict: PASS.** End-to-end against Gmail is **UNVERIFIED** — no credentials — but the
mechanism, the encoding and the TLS modes are all confirmed, which is the part that
would have failed.

---


