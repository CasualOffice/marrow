# Marrow — Decision: the MLX runtime

## Keep the Python sidecar, or replace it?

**Status:** Decision. Not specification — nothing here adds a requirement.
**Date checked:** 2026-08-30. Every star count, commit date, licence and version below was read
from the GitHub REST API, crates.io, PyPI or raw source on that date, and will be stale within weeks.
**Question:** `docs/Comparison.md` §13.7 and §9 argue that `crates/model/worker/mlx_worker.py` is
the wrong shape — that `omlx` already does Part 8's supervisor, that three teams wrote real Rust
bindings where Marrow shelled out, and that only the circuit breaker is unmatched. This tests those
three claims against the repositories themselves.

---

# 1. The decision

**Keep the sidecar. Bundle its venv. Add `omlx` and Ollama as detected R1 servers for
generation only, never for embeddings. Do not write or adopt Rust MLX bindings.** The reasoning is
short: every one of the three "real Rust bindings" is vendored inside an application and **none of
them is published** — mlxcel's `mlxcel-core` and rMLX's `rmlx-*` crates are absent from crates.io by
deliberate `publish = false`, Ollama's is Go and lives in its `x/` experimental namespace, and the
one binding that *is* on crates.io, `mlx-rs`, has an empty KV cache, two model architectures, and
`// pub mod generate;` commented out on `main`. There is nothing to link against. Meanwhile the
thing the sidecar sits on — `mlx` and `mlx-lm`, both MIT, both from Apple's `ml-explore`, both
pushed the morning this was written — is the most maintained software in this entire comparison, and
`omlx` itself depends on exactly those two packages. Marrow and the 21,000-star project it is
supposedly rebuilding are running the same code; the difference is that Marrow calls it in-process
and omlx calls it behind an HTTP API that **cannot express the one thing Marrow's embedder needs**
(§4.4). For one person who wants this working in two years, the safe dependency is Apple's, not a
six-month-old alpha with a bus factor of one.

---

# 2. What was checked

Read directly from the GitHub API, crates.io, PyPI, and raw source files, 2026-08-30.

| Project | ★ | Lang | Licence | Default-branch HEAD | Created | Contributors (top) |
|---|---|---|---|---|---|---|
| [ml-explore/mlx](https://github.com/ml-explore/mlx) | 28,221 | C++ | MIT | 2026-08-30 | 2023-11-28 | Apple |
| [ml-explore/mlx-lm](https://github.com/ml-explore/mlx-lm) | 6,839 | Python | MIT | 2026-08-30 | 2025-03-11 | Apple |
| [ml-explore/mlx-c](https://github.com/ml-explore/mlx-c) | 234 | C++ | MIT | 2026-08-28 | 2023-12-12 | Apple |
| [ml-explore/mlx-swift](https://github.com/ml-explore/mlx-swift) | 2,008 | C++/Swift | MIT | 2026-08-27 | 2023-12-12 | Apple |
| [ml-explore/mlx-swift-examples](https://github.com/ml-explore/mlx-swift-examples) | 2,656 | Swift | MIT | 2026-07-20 | — | Apple |
| [Blaizzy/mlx-embeddings](https://github.com/Blaizzy/mlx-embeddings) | 438 | Python | *(unrecognised)* | **2026-05-13** | 2024-07-16 | 1 primary |
| [jundot/omlx](https://github.com/jundot/omlx) | **20,992** | Python + Swift | Apache-2.0 | 2026-08-29 | **2026-02-13** | 100+, `jundot` 1,673 of ~2,530 commits |
| [lablup/mlxcel](https://github.com/lablup/mlxcel) | 383 | Rust | Apache-2.0 | 2026-08-30 | 2026-01-30 | 8, `inureyes` 1,408 |
| [oxiglade/mlx-rs](https://github.com/oxiglade/mlx-rs) | 370 | Rust | Apache-2.0 OR MIT | **2026-03-27** | 2023-12-23 | 9, `minghuaw` 302 |
| [Pushkinist/rMLX](https://github.com/Pushkinist/rMLX) | 12 | Rust | MIT OR Apache-2.0 | 2026-08-24 | 2026-06-06 | 2 (one is dependabot) |
| [robertelee78/mlx-native](https://github.com/robertelee78/mlx-native) | 4 | Rust | MIT AND Apache-2.0 | 2026-08-26 | 2026-04-07 | **1** |
| [ollama/ollama](https://github.com/ollama/ollama) | 179,764 | Go | MIT | 2026-08-29 | 2023-06-26 | large |
| [huggingface/candle](https://github.com/huggingface/candle) | 20,972 | Rust | Apache-2.0 | 2026-08-29 | 2023-06-19 | large |

Package registries:

| | Latest | Published |
|---|---|---|
| PyPI `mlx` | 0.32.2 | 2026-08-25 |
| PyPI `mlx-lm` | 0.31.3 | **2026-04-22** |
| PyPI `mlx-embeddings` | 0.1.0 | **2026-03-24** |
| crates.io `mlx-rs` | 0.25.3 | **2025-12-16** |
| crates.io `mlx-lm` (the Rust one) | 0.0.1 | 2025-10-04 · **738 downloads, lifetime** |
| crates.io `mlx-native` | 0.15.1 | 2026-08-26 · 2,531 downloads |
| crates.io `mlxcel-core`, `rmlx-core`* | — | **does not exist** |

\* `rmlx-core` 0.1.0 exists on crates.io from 2026-03-16 and is *not* rMLX — the rMLX workspace sets
`publish = false` on every member with the comment *"Internal app workspace — no member crate is
published to crates.io."* Do not mistake the squatted name for the project.

---

# 3. The three claims, tested

## 3.1 "omlx already implements almost everything Part 8's supervisor specifies"

**The substance is right. The implication is wrong.**

The star count checks out: 20,992, close enough to the claimed ~20,988. And `omlx/engine_pool.py`
really is a line-by-line match for §142 in the places that matter — read directly:

```python
    load_failed: bool = False   # Sticky until the next discovery refresh
    is_pinned: bool = False     # Never evict if True
    in_use: int = 0             # in-flight lease count; never evict while > 0
```

with `_current_ceiling`, `_fallback_admission_ceiling`, `_admission_soft_target` ("soft watermark
that pre-load eviction targets"), per-model TTL, and an explicit *"Pre-load memory checking (evict
before load, not after)"*. There is a `tests/test_scheduler_admission.py`, a `memory_monitor.py`, a
`process_memory_enforcer.py`. On idle eviction, admission against live memory, and multi-model
residency, omlx is ahead of `crates/model/src/supervisor.rs` and will stay ahead.

But **omlx is not code Marrow can take.** It is a Python package (`pyproject.toml`, `pip install
-e .`), a Homebrew formula, and a SwiftUI menu-bar app shipped as a `.dmg`. It publishes no library,
no crate, no C ABI. The only surface is HTTP — OpenAI- and Anthropic-compatible routes under
`omlx/api/`. "omlx already does this" therefore does not mean *delete `supervisor.rs`*; it means
*run a second 21,000-star application and talk to it over a socket*.

And what that application is standing on is worth reading before adopting it:

```toml
"mlx==0.32.0",
"mlx-lm @ git+https://github.com/ml-explore/mlx-lm@ab1806e8f5d6aa035973af194a1b9198ab4754dc",
"mlx-embeddings @ git+https://github.com/Blaizzy/mlx-embeddings@32981fa4…",
"transformers>=5.12.1,<5.13",
"nanobind==2.13.0",   # ABI-coupled to the mlx build
```

omlx pins `mlx` to an **exact patch version** because its custom Metal kernels are ABI-coupled to
it, and pins `mlx-lm` and `mlx-embeddings` to git SHAs. Classifier: `Development Status :: 3 -
Alpha`. Created six months ago. The project is excellent and it is not a stability bet — its
own dependency block is a more brittle environment than the one Marrow asks the user to `pip
install`, and it is running the same two libraries the sidecar imports.

## 3.2 "Three other teams have written real Rust bindings to MLX"

**Three teams wrote real bindings. None of them wrote a Rust binding you can use, and one of them
isn't Rust.** Verified, project by project:

| | What it actually is | Linkable? |
|---|---|---|
| **Ollama** | Real, substantial, and **Go**. `x/mlxrunner/mlx/` holds CGo over vendored `mlx/c` headers with `generated.c`/`generated.h` emitted by an in-tree generator, plus `MLX_VERSION` and `MLX_C_VERSION` pin files and its own `cache_trie.go`, `cache/rotating.go`, `cache/snapshot_capture_test.go`. It lives under `x/` — Ollama's experimental namespace | No. Wrong language, and a server either way |
| **mlxcel** | Real, Rust, `cxx` + `cxx-build` + `cmake` against MLX **C++**, vendored with in-tree overlays and an MLX commit pin | **No.** `mlxcel-core` is not on crates.io. Ships two binaries |
| **rMLX** | Real, Rust. `rmlx-mlx`: *"bindgen wrapper around brew-prebuilt mlx-c + safe Rust layer"*, a `build.rs` resolving `brew --prefix` and a `mlx-pin.txt` | **No.** `publish = false` workspace-wide |
| **mlx-native** | Real, Rust, but a *kernel* library: *"no `Tensor` type, no `Module` system, no model zoo… the consumer builds the actual transformer forward pass"* | Yes, and useless — you would write the model |

The comparison read this as *"shelling to Python is the option none of them took."* The correct
reading is the inverse: **writing an application-private FFI layer is the option all of them took,
and none of them will support yours.** Four teams, four private bindings, zero published crates. On
a repository where the author is one person, adopting an unpublished vendored binding means
vendoring it — which is not "using someone else's code", it is inheriting it.

The one binding that *is* published, `mlx-rs`, was checked in detail because it is the only real
candidate. Read from `main`:

- **`mlx-lm/src/lib.rs` line 3: `// pub mod generate;`** — the generation module is commented out.
- `mlx-lm/src/models/` contains exactly `llama.rs` and `qwen3.rs`. Not Qwen 3.5, not Gemma.
- `mlx-lm/src/cache.rs` defines a `KeyValueCache` trait, a `ConcatKeyValueCache`, and
  `pub struct DefaultKeyValueCache {}` — an empty struct. No rotating cache, no trim, no quantized
  cache, no prompt trie, no prefix reuse of any kind.
- The `mlx-lm` crate is version **0.0.1**, published once in October 2025, 738 lifetime downloads.
- No embeddings. Nothing resembling a SentenceTransformers dense head.
- `mlx-sys` pins the `mlx-c` submodule at `a1290d22` — **v0.5.0** (2026-02-06). Current is v0.6.0.
  (The comparison said 0.4.0; it is off by one release. A dependabot branch for the bump sits
  unmerged.)
- `main`'s HEAD is **2026-03-27**, five months back, with 92 open issues. The GitHub API reports a
  push on 2026-08-30, which is why it can look alive — that push is to a branch named
  `mlx-rs-verification-harness`, not `main`.

`mlx-rs` cannot generate text today. It is not a replacement for the sidecar; it is the first
quarter of writing an inference engine.

**Candle** was checked for completeness and rules itself out on format, not maintenance:
`candle-core/src/quantized/` is `ggml_file.rs`, `gguf_file.rs`, `k_quants.rs` — llama.cpp's
quantisation, not MLX's affine group quantisation. It cannot load an `mlx-community` 4-bit
checkpoint. Under LLM-037 that is a different registry entry with a different SHA, so "switch to
candle" is really "switch to GGUF", which is R4, which is already ranked below MLX in §139.1.1 and
for the stated reason.

**mlx-swift** deserves a mention it does not get in the comparison: `ml-explore/mlx-swift` (2,008★,
MIT, Apple) and `mlx-swift-examples` (2,656★, MIT, last pushed 2026-07-20) are the *only*
non-Python MLX stack with a real maintainer behind it, and `mlx-swift-examples/Libraries` carries
actual LM and embedding model code. A Swift sidecar speaking the same JSON Lines protocol is the
one credible "not Python" option. It is still a sidecar, still a build toolchain the user needs, and
it trades Apple's Python for Apple's Swift — a lateral move, and it is listed in §6 rather than
recommended.

## 3.3 "Only the persisted circuit breaker is unmatched"

**Understated.** The breaker is unmatched — omlx's nearest equivalent is the `load_failed` flag
above, sticky only until the next discovery refresh, with no ladder, no cooldown, no persistence,
and no distinction between the first error and the eighth. `crates/model/src/breaker.rs` is better
and its comment explaining why cooldown expiry does not reset the count is the kind of reasoning
nothing else in the field bothers with.

But three other things are also unmatched, and two of them are load-bearing:

1. **`kv.rs` scopes cache entries by `(workspace, classification)`** and refuses a hit across a
   classification boundary (LLM-043), with `invalidate_workspace` so a tightened classification does
   not cost every other workspace its cache. No server in this field has a concept of a workspace
   classification — they optimise for the opposite, sharing prefixes as widely as possible. This is
   policy code, not runtime code, and it is exactly the thing §14 of the comparison identifies as
   the real differentiation.
2. **Per-text unpadded embedding.** See §4.4 — this is the finding that decides the whole question.
3. **`cacheTrimmable` / `cacheKinds` reported at load** (LLM-060). No HTTP API exposes it, because
   no HTTP API has a reason to: for a server, a re-prefill is a latency number, and for Marrow it is
   the difference between a follow-up reusing 80% of its prompt and reusing none of it.

---

# 4. What the sidecar does that a replacement must also do

Five behaviours, each present in `mlx_worker.py` for a written reason, checked against every
candidate.

## 4.1 Chat-template application, as token ids

`_as_chat_turn` calls `apply_chat_template` and then keeps **token ids**, not rendered text, because
the KV cache is keyed on the exact token sequence and re-encoding a rendered template is a second
chance to disagree with the first.

An OpenAI-compatible server takes `messages` and templates server-side. Marrow never sees the ids,
so it cannot key its own cache on them, and `crates/model/src/kv.rs` — the classification-scoped
prefix cache — has nothing to hash. Through a server, LLM-040 through LLM-045 become the server's
business and LLM-043 becomes unimplementable. **Not replaceable.**

## 4.2 `enable_thinking`

omlx does support this: `ChatCompletionRequest` carries
`chat_template_kwargs: Optional[Dict[str, Any]]` with the comment *"e.g. enable_thinking,
reasoning_effort"*. **Replaceable** — this one is genuinely commodity.

The `<think>` splitting is a separate matter. `mlx_worker.py` holds back partial tags so `"<thi"` is
never emitted as answer text and retracted a frame later (GEN-014). omlx has an `omlx/api/thinking.py`
which I did not read; whether it holds back partial tags is unverified (§8).

## 4.3 `LRUPromptCache` prefix reuse

`_prepare_cache` uses `mlx_lm.models.cache.LRUPromptCache` with `fetch_nearest_cache` /
`insert_cache`, capped at 4 entries and 2 GB. Verified present in the released `mlx-lm` v0.31.3
(the `LRUPromptCache` refactor landed 2026-03-26; the release is 2026-04-22), so the sidecar is not
depending on unreleased upstream. In `mlx-rs`, as established, the equivalent is an empty struct.
Through omlx or mlxcel the reuse happens — better than Marrow's, with an SSD tier — but inside
their process, on their key, with no classification scope.

**Commodity as an optimisation; not commodity as a policy boundary.**

## 4.4 Per-text unpadded embedding — the finding that settles it

`_embed`'s docstring records a measurement: with `mlx_embeddings` and EmbeddingGemma, the word
`"short"` embedded beside a 40-token passage agrees with itself embedded alone at only **0.89**. The
pooling masks padding correctly; the attention leaks. So the sidecar embeds one text at a time and
batches only at the IPC layer.

Now read omlx's embedding path. `omlx/engine/embedding.py`:

```python
    async def embed(self, ..., max_length=None, padding: bool = True, truncation: bool = True):
        ...
        # A batch pads every input to its longest member, so a batch of mixed lengths spends
        # most of its compute on padding. Group similar lengths together and restore the …
```

It pads by default, batches at 32, and **length-sorts the inputs** to make the padding cheaper.
Then `omlx/api/embedding_models.py`:

```python
class EmbeddingRequest(BaseModel):
    input: ...; items: ...; model: str
    encoding_format: ...; dimensions: ...; max_length: ...; truncation: bool = True
```

There is **no `padding` field**. The API cannot turn it off.

So routing embeddings through omlx reintroduces the exact defect the sidecar was written to avoid,
and makes it worse: because inputs are length-sorted into batches, a chunk's stored vector depends
on which *other* chunks happened to be in the same request. Re-embed the same corpus in a different
order and the index moves. That is Marrow's own hard rule about derived indexes being rebuildable
turned into a liability — a rebuild would not reproduce the index.

The only workaround through the HTTP API is one text per request, which over ingest is one round
trip per chunk. **Not replaceable, at any price.** Whatever else changes, `embed` stays in-process.

## 4.5 Reporting whether the cache can be trimmed

`op_load` probes `can_trim_prompt_cache(make_prompt_cache(model))` and reports `cacheTrimmable` and
`cacheKinds`. LLM-060 exists because Qwen 3.5 4B mixes `ArraysCache` with `KVCache` and returns
false — on precisely the model this is built for. No server exposes this. **Not replaceable**, and
losing it would turn a known architectural property into an unexplained speed difference.

---

# 5. What is genuinely commodity, and should go

Named specifically, with the owner.

| Part 8 | Why it should go | Whose code instead |
|---|---|---|
| **LLM-046** — quantized KV cache "offered where the runtime supports it, off by default" | A quality/memory trade with a UI surface, for a machine with one model loaded. Every server here does it better and Marrow has no way to evaluate the quality cost | mlx-lm's `QuantizedKVCache`, if ever needed. Otherwise nothing |
| **§139.1 R3** — "embedded Rust runtime" | Verified impossible at acceptable cost. There is no publishable Rust MLX binding; `mlx-rs` cannot generate. R3 is a placeholder for work nobody has done | Delete the row |
| **§139.1 R4** — llama.cpp via bindings, "GGUF the other runtimes lack" | R1 already covers this. An installed Ollama or LM Studio *is* the llama.cpp binding, with a maintainer | R1 |
| **Multi-model residency** in the supervisor | Marrow holds one embedder and at most one generator (§139.4, LLM-050). Pinning, leases, and LRU across N models is omlx's problem, not a 16 GB Mac's | omlx's `engine_pool.py`, if the shape is ever needed |
| **Continuous batching, speculative decoding, SSD-tiered KV** | Never specified in Part 8 and should stay unspecified. Single-user, one request at a time | mlxcel / omlx |
| The *idle-eviction and memory-watermark* half of §142.3 | Commodity, and Marrow's version will stay behind omlx's | Keep Marrow's anyway — see §6 |

The last row is deliberately contradictory and it is the honest answer: the mechanism is commodity,
but the thing Marrow needs it for is not, and it is ~200 lines. Rewriting `admission.rs` to call an
HTTP server that has its own opinion about admission buys nothing.

---

# 6. What is not commodity, and stays

| | Why |
|---|---|
| **`breaker.rs`** — persisted, per-model, laddered, first-error-preserved | Nothing in the field has it. `crates/model/src/breaker.rs`, 228 lines, done |
| **`kv.rs`** — prefix cache scoped by `(workspace, classification)` | LLM-043 is a policy boundary implemented as a cache key. Every other project's prefix cache is designed to share as widely as possible, which is the opposite requirement |
| **`embed.rs` + `_embed`** — per-text, unpadded, width measured at load, order-preserving with a length check | §4.4. The one place where the field's default behaviour is measurably wrong for Marrow's use, and the API surface of every server makes it unreachable |
| **`cacheTrimmable` reporting** | LLM-060. A model property that decides observable behaviour, reported rather than discovered |
| **The `<think>` wire split with partial-tag hold-back** | GEN-014/GEN-015. Thinking is never citable evidence, so the split has to be structural, not a UI regex |
| **`worker.rs`'s supervision** — 20 s handshake, protocol-version refusal, stdout-on-its-own-thread so a deadline is enforceable, stderr drained so a full pipe never stalls the model | LLM-020 through LLM-023. This is the part that would have to be rewritten *identically* for any sidecar in any language, so it is not a cost of keeping Python |
| **`admission.rs`'s policy refusals** | "Resource refusals are overridable; policy refusals are not" (MOD-004). No server distinguishes these, because no server has a workspace classification |

---

# 7. The options, and what each actually costs

## 7.1 Do nothing

**Cost:** the user creates a venv by hand, and the printed instruction is wrong. `Runtime::setup_hint`
says `pip install mlx-lm`; `op_load` imports `mlx_embeddings` when `kind == "embedding"`. Following
the hint exactly produces a runtime that generates and cannot embed — that is, search enrichment
fails on a fresh machine while `ask` works, which is the confusing direction for it to fail in.

**Second cost:** nothing is pinned. `pip install mlx-lm` today gets 0.31.3, which has
`LRUPromptCache.fetch_nearest_cache`. PyPI `mlx-lm` has been static since 2026-04-22 while the
repo is pushed daily — omlx pins a git SHA for exactly this reason. One day a `pip install` in a
fresh venv gets a version where that API moved, and the failure is an ImportError at load with no
version number in it.

**Third cost, and the real one:** `mlx-embeddings` is the least-maintained thing in the stack —
438★, one primary maintainer, last push 2026-05-13, PyPI 0.1.0 from 2026-03-24, and a licence
GitHub cannot classify. `mlx` and `mlx-lm` are Apple's and are fine for years. `mlx-embeddings` is
one person's. **This is the dependency to worry about, and it is one import in one function, not the
sidecar architecture.**

## 7.2 Keep the sidecar, bundle the venv — **recommended**

**Changes:** `crates/model/src/worker.rs` (`Runtime::discover` / `setup_hint`),
`crates/desktop/src/models.rs` (the hint assertion at ~:1252 and the script-path resolution at
~:1096), and a build or first-run step that creates `runtime/mlx` and installs pinned versions.

**Cost:** a first-run download of ~450 MB and a network dependency the README must state honestly
(§13.7 of the comparison is right that "one binary plus a parser subprocess" is no longer true, and
bundling does not make it true — it makes it *automatic*, which is the achievable half).

**Gains:** the hint stops being wrong, the versions stop floating, and the third runtime becomes an
install step rather than a manual prerequisite. Nothing stops working. All eleven `#[ignore]` tests
keep working unchanged.

## 7.3 Replace with Rust bindings

**Changes:** `worker.rs` (1,618 lines) and `mlx_worker.py` (443) deleted; `embed.rs` rewritten;
`backfill.rs`, `crates/desktop/src/models.rs`, `ask.rs`, `commands.rs`, and both examples follow.

**What stops working:** everything in §6 that depends on `mlx_lm` — chat templating, `enable_thinking`,
`LRUPromptCache`, `can_trim_prompt_cache`, and embeddings entirely, since no Rust binding has an
embedding path at all. On `mlx-rs` specifically, generation also stops working, because
`pub mod generate` is commented out.

**What the user installs instead of a venv:** Homebrew `mlx-c`, or a vendored MLX C++ tree and a
CMake toolchain in the build.

**What happens to the tests:** the eleven `#[ignore]` tests (five in `worker.rs`, three in
`embed.rs`, two in `backfill.rs`, one in `download.rs`) are all written against the JSON Lines
protocol and all get deleted. With them go the measurements they encode — the 0.89 padding figure,
the ~80% prefix reuse on Qwen 3 0.6B, `can_trim_prompt_cache` false on Qwen 3.5 4B. Those numbers
took real model runs to obtain and are the empirical basis for LLM-060 and §4.4.

**Verdict: no.** This is writing an inference engine, in a repository whose scope discipline says the
default answer to "should we also…" is no.

## 7.4 Run against a local Ollama / omlx server

**Changes:** a new `GenerationProvider` implementation and an HTTP client; `worker.rs` stays for
embeddings.

**What stops working:** LLM-043 (no token ids, so no classification-scoped prefix cache), LLM-060
(not exposed), and the embedding path outright (§4.4 — omlx's `EmbeddingRequest` has no `padding`
field). Marrow's own admission and breaker become advisory, since the server admits or refuses on
its own terms and Marrow cannot see its memory accounting.

**What the user installs:** a `.dmg` or a brew tap, plus a running background service. On the "two
years, no maintenance" test, omlx is six months old, alpha-classified, and pins `mlx` to an exact
patch version against ABI-coupled Metal kernels; Ollama is enormously maintained but its MLX runner
is in `x/`.

**Verdict: yes, narrowly, and it is already in the spec.** §139.1's **R1** says an already-installed
Ollama or LM Studio is the first choice — *"zero bytes downloaded, zero maintenance, and the user
already chose those models."* omlx belongs on that list. The change is to `detect.rs`, not to
`worker.rs`, and it is generation-only.

## 7.5 Hybrid — the recommendation, stated as a rule

> **Generation may come from anywhere. Embeddings come from the sidecar.**

The generator is one model, called interactively, whose output is text a human reads. If a detected
omlx or Ollama serves it, the cost is a worse prefix cache and a lost `cacheTrimmable` report, and
the gain is that a user who already runs one of those downloads nothing. The embedder is called
tens of thousands of times, its output is stored, and a vector that depends on its batch mates
corrupts the index invisibly and permanently. Those are not the same decision and should stop being
made together.

A Swift sidecar over `mlx-swift` (§3.2) would satisfy the same rule and is the only credible way to
drop Python. It is not recommended now — it trades Apple's Python for Apple's Swift plus a Swift
toolchain, and buys nothing on this list — but it is the option to revisit if
`mlx-embeddings` is ever abandoned, because `mlx-swift-examples/Libraries` has embedding model code
with Apple behind it.

---

# 8. What could not be verified

Listed rather than glossed.

- **The 0.89 self-agreement figure and the ~80% prefix-reuse figure** are Marrow's own measurements,
  recorded in `_embed`'s docstring and in LLM-060. I read the code that asserts them; I did not
  re-run them. The mechanism (omlx pads and length-sorts) is verified from source; the magnitude on
  EmbeddingGemma is taken from Marrow's own record.
- **Whether omlx's `omlx/api/thinking.py` holds back partial `<think>` tags** — not read. §4.2's
  claim is limited to `chat_template_kwargs`, which is verified.
- **mlxcel's and rMLX's embedding padding behaviour** — not read. They are ruled out on
  publishability, not on this, so it did not change the conclusion.
- **`mlx-native`'s actual coverage** beyond its README's own trade-offs section. It rules itself out
  in that section ("no `Tensor` type, no `Module` system, no model zoo"); I did not audit the claim.
- **Ollama's MLX runner maturity.** I confirmed the files exist (`x/mlxrunner/mlx/generated.c`,
  vendored `mlx/c` headers, `MLX_VERSION` / `MLX_C_VERSION` pins, `cache_trie.go`,
  `cache/rotating.go`) and that it sits under `x/`. I did not confirm which models it serves or
  whether it is enabled by default in a release build.
- **Whether omlx's HTTP API can be persuaded to return token ids** for a templated prompt. I read
  `ChatCompletionRequest` and found no such field, but I did not read every route under
  `omlx/api/`. If one exists, §7.4's LLM-043 objection weakens.
- **A correction to a source used here:** a fetched summary of rMLX's README stated that it wraps
  MLX *"through the `oxideai/mlx-rs` community Rust binding."* Its `Cargo.toml` has `bindgen` as a
  build-dependency and no `mlx-rs` dependency at all, and `rmlx-mlx`'s own description says
  *"bindgen wrapper around brew-prebuilt mlx-c."* The summary was wrong; the manifest is what is
  reported above.
- **`cargo test` was not run.** Another process holds the tree. Test counts and `#[ignore]`
  locations are from reading the sources.
- The **contributor counts** are from GitHub's contributors endpoint, which caps at 100 and counts
  commits, not review or maintenance. For omlx it reports 100+ contributors with 1,673 of ~2,530
  commits by one account; treat "bus factor of one" as an inference from that ratio, not a measurement.
