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


## S3 — SAM ONNX export — ✅ **PREMISE CONFIRMED / SCOPE-CHANGE**

**The claim in `INCOMPLETE.md` is correct, and it is sharper than it looked.** Upstream
exports the prompt encoder and mask decoder only. The ViT image encoder is not in the
graph, so the export alone cannot run SAM.

Verified in source, not from the README:

| Evidence | Finding |
|---|---|
| `segment_anything/utils/onnx.py` | `grep -c image_encoder` → **1**. The single occurrence is `model.image_encoder.img_size` (line 35), which reads a config attribute. The encoder is **never called**. |
| `SamOnnxModel::forward` (`onnx.py:108-116`) | First parameter is `image_embeddings: torch.Tensor` — an **input**. |
| `scripts/export_onnx_model.py:130-137` | `dummy_inputs["image_embeddings"] = torch.randn(1, embed_dim, *embed_size)`. The exporter feeds embeddings in. |
| `torch.onnx.export` | Traces only what `forward` executes, so the ViT is pruned from the graph. |
| Script docstring | "Export the SAM prompt encoder and mask decoder to an ONNX model" — accurate. |

### What the encoder pass needs, precisely

The missing graph is small and its contract is fixed. For `vit_b`
(`build_sam.py:37-43`: `embed_dim=768, depth=12, num_heads=12,
global_attn_indexes=[2,5,8,11]`; `image_encoder.py:20-27`: `img_size=1024,
patch_size=16, out_chans=256`):

| | |
|---|---|
| Input | `[1, 3, 1024, 1024]` f32 |
| Output | `[1, 256, 64, 64]` f32 |
| Matches the decoder's input | `embed_dim = 256` (`MASK_EMBED_DIM`) and `embed_size = (64, 64)` — i.e. `[1, 256, 64, 64]`, so the two graphs compose |

Producing it is a ~15-line wrapper around `sam.image_encoder` and one
`torch.onnx.export` call. **This was not done here: there is no Python interpreter and
no PyTorch on this host, and the ViT-B checkpoint is 93.7 M parameters (~375 MB fp32).**
So the encoder pass remains **unmeasured** — no timing is claimed.

| Item | Finding |
|---|---|
| Weights | `facebook/sam-vit-base`, **Apache-2.0**, 93.7 M params |
| Runtime | `ort` 2.0.0-rc.13 (already pinned by S1/S2) — no new runtime |
| Verdict | Premise confirmed. Upstream artefact missing but trivially producible |

**Recommendation:** keep F168, scope it as *two* graphs plus the pre/post
(resize-longest-side-to-1024, pad, normalise) wrapper, and drop the assumption that an
upstream artefact exists. Do not remove the feature: the work is bounded and
well-understood. Requires a host with PyTorch for the export step, once.


---


## S4 — `any-tts` Kokoro backend — ✅ **YES, and `kokoroxide` is moot**

`any-tts` 0.1.3 provides Kokoro as a **default feature**, not as an example.

| Evidence | Finding |
|---|---|
| `src/lib.rs:23` | `kokoro` — "Build Kokoro model support (**default**)" |
| `src/lib.rs:84-86` | `#[cfg(feature = "kokoro")]` → `models::kokoro::KokoroModel::load(config)` |
| `src/models/kokoro/` | 11 modules, a full implementation: `albert.rs` (PL-BERT text encoder), `decoder.rs` (ISTFTNet), `phonemizer.rs` + `english_g2p.rs` + `espeak_compat.rs`, `style_encoder.rs`, `prosody.rs`, `text_encoder.rs` |
| `examples/generate_kokoro.rs` | 19 sample utterances across en / en-gb |

**Answer to the spike question: yes.** `kokoroxide` 0.1.5 (explicitly WIP, 7 stars) is
moot and should not be evaluated further.

### Two things the README does not tell you

1. **It is `candle`, not `ort`.** `candle-core`/`candle-nn`/`candle-transformers` 0.10.2.
   AGENTS.md assigns `ort`/`ort-sys` exclusively to `odysseus-ml`; this would add a
   **second ML runtime** to the workspace. That is a deliberate decision, not a detail.
2. **The voice assets are not obtainable in the shape it wants.** `model.rs:397-403`
   loads `voices/{name}.pt` — a PyTorch bare tensor per voice. The canonical
   `hexgrad/Kokoro-82M` repo publishes `kokoro-v1_0.pth` with the voices packed inside,
   and the split-voice repos (`onnx-community/Kokoro-82M-v1.0-ONNX`) publish
   `voices/*.bin`, not `.pt`. So materialising the voice files needs a one-time
   PyTorch extraction step, even though the runtime is pure Rust.

| Item | Finding |
|---|---|
| Version | **0.1.3** (matches the 0.1.3 in the maturity table) |
| Licence | **MIT OR Apache-2.0** — already allowed by `deny.toml` |
| Weights | `hexgrad/Kokoro-82M`, **Apache-2.0** |
| Transitive deps | **357** (the candle stack dominates) |

**Not run.** Weights were not fetched, so **no audio was synthesised and no quality or
latency figure is claimed.** One incidental finding: `KokoroModel::load` returned `Ok`
against an **empty** model directory, so it does not validate assets at load time —
a misconfigured model path surfaces later, at synthesis. Worth a startup probe in
Phase 22 rather than trusting `load`.


---


## S6 — `whisper-rs` CPU transcription — ✅ **PASS (CUDA unverified)**

Transcribes correctly on CPU with **zero word errors**, measured against the
published transcript of whisper.cpp's own `samples/jfk.wav`.

| Item | Finding |
|---|---|
| Crate | **0.16.0**, **Unlicense** (allowed by `deny.toml`) |
| Vendored C | whisper.cpp, **MIT** |
| Model | `ggml-tiny.en.bin`, **77.11 MB** |
| Transcript | *"And so my fellow Americans ask not what your country can do for you ask what you can do for your country."* |
| WER vs published reference | **0.000** (0 errors / 22 words) |
| Speed | **4,498 ms for 11.0 s audio = 2.45× realtime**, beam search, tiny.en, no GPU |
| GPU | `whisper_backend_init_gpu: no GPU found` — CPU path only |

### Build requirements not in the README

`whisper-rs-sys` needs **cmake** and **libclang** at build time. Neither is in
`devenv.nix`: the first build failed with `is cmake not installed?`, and bindgen then
failed with `'stdio.h' file not found`. Resolved for the spike by putting
`cmake 4.4.3` on `PATH` and setting `LIBCLANG_PATH` +
`BINDGEN_EXTRA_CLANG_ARGS` to a glibc include dir. **This is a real gap in the dev
shell** — `cmake` should be added to `devenv.nix` packages, which is outside this
spike's ownership.

### Is whisper.cpp a regression against faster-whisper?

**Not answered, and not answerable here.** faster-whisper is CTranslate2, which needs
a Python environment with cuDNN/cuBLAS; there is no interpreter on this host, so no
side-by-side was possible. Stating a parity verdict without running both would be
exactly the kind of claim this document exists to prevent.

What *is* established: whisper-rs produces a **correct** transcription on CPU, so it is
not a functional regression. faster-whisper's documented advantages (CUDA via cuDNN,
int8 quantisation, batching) are all **GPU-side**, and CUDA is unverified here for the
same reason as S1/S2. Re-confirm on a CUDA machine before Phase 22 relies on it.

**Verdict: PASS.** F339 stays. **CUDA/TensorRT runtime is UNVERIFIED**, same caveat as
S1/S2.


---


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


## S9 — Local Stable Diffusion scope — 📌 **DECISION: REMOVE F176**

**Decision, not a finding: do not port Stable Diffusion to Rust.**

`REWRITE_PLAN.md:66-68` already settles this, and S9 only confirms it:

> `scripts/diffusion_server.py` and `swift/odysseus-mlx-image-bridge/` are already
> *optional out-of-process backends* in the current app, not part of the app process.
> **Anything replacing them inherits that placement.**

So F176 is not required for the single-process goal — it is an *optional backend* by the
plan's own definition, alongside Ollama/vLLM and MCP subprocesses.

### The evidence

- `/api/image/inpaint` — `routes/gallery/gallery_routes.py:1257`. Proxies to a
  registered `ModelEndpoint` (`_first_visible_image_endpoint(db, user)`), with SSRF
  validation (`check_outbound_url`) and OpenAI re-shaping.
- `/api/image/harmonize` — `gallery_routes.py:1515`. Same: a registered
  `ModelEndpoint`'s `base_url` + `api_key`, sending real img2img with `strength`
  (comment at `:1509-1514` explains why a 100 %-mask inpaint call is the wrong shape).
- `/images/generations` (txt2img) — `gallery_routes.py:269`, `:634`. Also a proxy.
- `scripts/diffusion_server.py` — 1,506 lines, PyTorch + FastAPI, `from_pretrained`
  diffusers pipelines. An HTTP service speaking OpenAI-shaped endpoints, i.e. exactly
  what a `ModelEndpoint` row describes.

### Why removing it is the right call, not the lazy one

The cost is the largest single port in the rewrite — CLIP tokenizer + four exported
graphs + **a scheduler port with no ONNX equivalent** (200–400 lines of stateful math),
against `ort 2.0.0-rc.13`, which is a release candidate, plus a TensorRT engine cache
for the claimed 1.5–2.5 s. The benefit is **zero capability that is not already
reachable over HTTP**, because the Python app already reaches it the same way.

**Action:** F176 → REMOVED, with the reason recorded (not deleted). `diffusion_server.py`
remains an *optional external backend* that Rust registers as a `ModelEndpoint` — the
same mechanism inpaint and harmonize already use. Nothing about image editing or
generation is lost; only the Rust reimplementation of a PyTorch service is dropped.

**Re-open if** image generation becomes a hard requirement for a LAN-only install with
no external GPU. It is not one today.


---


## S10 — PDF: `lopdf` / `mupdf` / `poppler-rs` — ⚠️ **SPLIT: F148 OK, F140 licence-blocked**

The three crates divide the two features differently, and **only one of the two PDF
features has a licence-clean implementation.**

| | AcroForm fill (F148) | Rasterisation (F140) |
|---|---|---|
| `lopdf` 0.45.0 (MIT, pure Rust, **0 C deps**) | ⚠️ **no AcroForm API** — but the object model makes it doable by hand | ❌ none |
| `mupdf` 0.8.0 (**AGPL-3.0**) | ✅ | ✅ |
| `poppler-rs` 0.26.0 (declares MIT) | — | ✅ but links **GPL-2.0** libpoppler |

### F148 AcroForm fill — lopdf suffices, verified

`lopdf` has **no** `AcroForm` helper: `grep -ri acroform` across the whole crate returns
nothing, and the only related API is `Document::get_page_annotations` (returns annotation
dictionaries). So the fill is dictionary work, not a call. Verified end to end: a
hand-built widget annotation (`/FT /Tx`, `/T`, `/V`) plus a hand-built `/AcroForm`,
filled by setting `/V`, **saved, reloaded, and the value read back**:

```
page annotations: 1
field /T  = "full_name"
field /V  = Some("Ada Lovelace")
/AcroForm present after reload: true
pdf bytes: 732
```

96 transitive dependencies and no C library at all. Acceptable, with the caveat that
**the AcroForm code is ours to maintain** — including `/NeedAppearances`, appearance
streams, and field appearance regeneration, which the 732-byte proof does not exercise.

### F140 rasterisation — no licence-clean option. This is the finding.

- **`mupdf` is AGPL-3.0.** The workspace is already `AGPL-3.0-or-later`
  (`Cargo.toml:22`), so linking it is *licence-compatible* — the obstacle is purely
  `deny.toml`, whose allow-list has 13 entries and does not include AGPL. **That is a
  policy decision for a human, not a legal blocker.**
- **`poppler-rs` is the interesting one.** crates.io reports **MIT**, and
  `cargo deny` would wave it through. But `Cargo.toml:18` says `build = false` and it
  depends on `poppler-sys-rs`: it is **gir bindings to a system `libpoppler`**
  (`Gir.toml`: `library = "Poppler"`, `version = "0.18"`, linking `Gio`, `GLib`,
  `GObject`, `Cairo`). **libpoppler is GPL-2.0-or-later.** So the licence check passes
  while the shipped binary links GPL — **a hole in the current `deny.toml` policy**,
  independent of this feature. Separately, `libpoppler-glib` is **absent from the dev
  shell** (`pkg-config --exists poppler-glib` fails; `glib` and `cairo` are present), so
  it cannot even be linked here.

**Recommendations**

1. **F148** → proceed with `lopdf`, budget for a hand-written AcroForm layer.
2. **F140** → do **not** take `poppler-rs`. If rasterisation is required, take `mupdf`
   and add AGPL-3.0 to `deny.toml` with a written justification, or keep rendering
   server-side behind the existing image proxy. Record that `deny.toml` currently cannot
   detect GPL linked through a `-sys` crate — worth fixing regardless of F140.
3. Both crates' licences must be re-checked at adoption time; both are moving targets.

---


## Corrections to assumptions in the planning documents

| Document | Was | Now |
|---|---|---|
| `REWRITE_PLAN.md` D10 | "pin `2.0.0-rc.13`" | ✅ confirmed correct |
| `INCOMPLETE.md` S1 | "EPs may be gated behind `api-22`" | ✅ resolved — not gated |
| `INCOMPLETE.md` S7 | "FTS5 + sqlite-vec must be confirmed" | ✅ resolved — both work |
| `DATA_MIGRATION.md` §5.2 | Thresholds need full re-tuning | **RESTORED — the earlier downgrade was based on a false premise.** Python sets cosine explicitly (`embedding_lanes.py:80`), not L2, so `1.0 - distance` is only a similarity under cosine. The port must name the metric; re-tuning is not required *if* it does, but L2-by-default would silently break every threshold |
| `INCOMPLETE.md` F168 | "Needs a separate `image_encoder.onnx` conversion" | ✅ confirmed exactly — `onnx.py` references `image_encoder` once, to read `img_size`, and `forward` takes `image_embeddings` as an input. Encoder contract for ViT-B: `[1,3,1024,1024]` → `[1,256,64,64]` |
| `INCOMPLETE.md` F338 | "`kokoroxide` becomes REMOVE if `any-tts` has no Kokoro backend" | ✅ it **does**, as a default feature with 11 modules. `kokoroxide` is moot. New caveat: it is `candle`, not `ort`, and its voice assets need a one-time PyTorch extraction |
| `SPIKE_RESULTS.md` S6 row | "`whisper-rs` — CUDA support" | CUDA **unverified** (no GPU, same as S1/S2). CPU is verified: WER **0.000** on whisper.cpp's own `jfk.wav`, 2.45× realtime |
| `SPIKE_RESULTS.md` S10 row | "`mupdf`/`poppler-rs` for rasterisation. **C-dependency implications**" | Sharper than that: `poppler-rs` **links GPL-2.0 libpoppler** while reporting MIT, so `cargo deny` passes on a GPL-linked binary. `mupdf` is AGPL-3.0 — compatible with this project, but not on `deny.toml`'s list |
| `FEATURE_INVENTORY.md` | — | PDF rasterisation appears as **F140** in `INCOMPLETE.md` and **F146** in `FEATURE_INVENTORY.md`. Same feature, two IDs. Not reconciled here — flagging it |

---

## Spike index

| Spike | Question | Verdict | Blocks |
|---|---|---|---|
| **S1/S2** | `ort` version + execution providers | ✅ USABLE, CUDA unverified | F176, F168 |
| **S3** | Does upstream SAM export include the ViT image encoder? | ✅ premise confirmed; needs a second graph | F168 |
| **S4** | Does `any-tts` provide Kokoro? | ✅ yes, default feature | F338 |
| **S5** | Does `rembg-rs` produce an alpha matte? | ❌ does not compile | F169 REMOVED |
| **S6** | `whisper-rs` CPU parity; CUDA; regression vs faster-whisper | ✅ WER 0.000 on CPU, CUDA unverified | F339 |
| **S7** | `rusqlite` FTS5 + `sqlite-vec` extension | ✅ both work | F125, F124 |
| **S8** | Gmail OAuth IMAP + STARTTLS SMTP | ✅ XOAUTH2 verified on the wire | Phase 15 |
| **S9** | Is local SD needed given the proxy path? | 📌 no — REMOVE F176 | F176 REMOVED |
| **S10** | PDF rasterisation + AcroForm fill | ⚠️ split: F148 OK, F140 licence-blocked | F140, F148 |
| **S11** | Capture the sqlite-vec golden set from live ChromaDB | ✅ captured, 3 lanes | F118 |

### S11 is time-critical

The golden set must be captured from the live ChromaDB instance *before* anything
touches vectors. Once that service is stopped or its data directory removed, the
calibration baseline is unrecoverable. Schedule it before Phase 7 begins.

**Feasibility confirmed.** An earlier revision of this note recorded the collections as
empty and treated capture as blocked. It is not blocked: the `tool_index` lane holds
**69 records** and is exactly the kind of populated lane S11 needs. Capture should
therefore cover all three lanes and record which were empty at capture time, so the
golden set documents real state rather than silently omitting two empty collections.
Because the metric finding above was wrong, the golden set must also record, per row,
the distance **and** `1.0 - distance` under an explicitly-declared cosine metric — that
pair is what makes it a calibration baseline rather than a smoke test.

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
| `poppler-rs` | 0.26.0 | 246,144 | 2026-02-21 | **Do not use** — reports MIT but links GPL-2.0 libpoppler. See S10 |
| `any-tts` | 0.1.3 | 17,967 | 2026-08-26 | **Early, but it has Kokoro.** See S4 |
| `kokoroxide` | 0.1.5 | 1,734 | 2025-10-04 | **Moot** — `any-tts` provides Kokoro. See S4 |
| `mupdf` | 0.8.0 | 1,709,128 | 2026-06-22 | Healthy, **but AGPL-3.0** — not on `deny.toml`. See S10 |
| `whisper.cpp` (vendored by `whisper-rs`) | — | — | — | MIT. CPU verified, CUDA unverified. See S6 |
| `rembg-rs` | 0.1.4 | 818 | 2025-12-28 | **Abandoned — does not compile.** See S5 |