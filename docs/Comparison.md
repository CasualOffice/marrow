# Marrow — Comparison

## What else exists, who does it better, and what is actually differentiated

**Status:** Assessment, not specification. Nothing here is a requirement.
**Date checked:** 2026-08-30. Every star count, commit date and licence below was
observed on that date and will be stale within weeks.
**Scope:** Marrow as *built* (`crates/`), not as documented. Where the code and
the docs disagree, §13 says so.

---

# 1. How to read this

Three grades of confidence are used, and the difference matters more than usual
here because most of this was gathered by delegated search:

| Grade | Meaning |
|---|---|
| **Verified** | Read directly — the repo's own source, or a raw file fetched from GitHub during this pass |
| **Reported** | Gathered by a research pass with a URL and a date, not independently re-read |
| **Unverified** | Could not reach it, or the sources disagree. Stated as unknown, never smoothed over |

Everything in §6 (write-tool safety) is **Verified** — it is the section most
likely to be quoted, so it was re-read from source rather than relayed. §8's
Ollama and omlx internals are **Reported**.

---

# 2. What Marrow is today

The comparison is worthless against the README, so here is the as-built state.

| | |
|---|---|
| Size | ~46,000 lines of Rust across 14 crates |
| History | 30 commits, all dated 2026-08-30 |
| Index | **SQLite FTS5**, not Tantivy (D3 reversed) — `unicode61 remove_diacritics 2`, `prefix='2 3'` |
| Spans produced | **`Bytes` and `Lines` only.** `Page`, `Cells`, `XPath`, `Time` are declared and have no producer |
| PDF | **Dropped** (D4 — 14 PDFs in the whole corpus) |
| Model runtime | Rust supervisor driving **`mlx_lm` in a Python subprocess** over JSON Lines |
| Surface | Tauri 2 + React desktop app, a CLI, and an MCP server |
| Adversarial corpus | 59 cases, 8 TOML files, data-driven, with a monotonic floor test |

The gap between that table and the README's `contract.pdf p17, ¶Renewal` is the
subject of §13.

---

# 3. The field

Only the projects that bear on an axis Marrow made a decision about. Counts and
dates from 2026-08-30.

## 3.1 Local document Q&A over your own files

| Project | ★ | Lang | Licence | Last activity | Grade |
|---|---|---|---|---|---|
| [AnythingLLM](https://github.com/Mintplex-Labs/anything-llm) | 65,374 | JS | MIT | 2026-08-28 | Reported |
| [docling](https://github.com/docling-project/docling) | 65,737 | Python | MIT | 2026-08-28 | Reported |
| [Khoj](https://github.com/khoj-ai/khoj) | 36,791 | Python | AGPL-3.0 | 2026-08-02; last release 2026-03-26 | Reported |
| [cognee](https://github.com/topoteretes/cognee) | 30,344 | Python | Apache-2.0 | 2026-08-24 | Reported |
| [kotaemon](https://github.com/Cinnamon/kotaemon) | 25,723 | Python | Apache-2.0 | 2026-05-30 | Reported |
| [SurfSense](https://github.com/MODSetter/SurfSense) | 16,032 | Python | Apache-2.0 + BUSL-1.1 | 2026-08-29 | Reported |
| [LEANN](https://github.com/yichuan-w/LEANN) | 12,845 | Python | MIT | 2026-08-30 | Reported |
| [Reor](https://github.com/reorproject/reor) | 8,567 | JS | AGPL-3.0 | **2025-05-13 — archived** | Reported |

Onyx, RAGFlow, PrivateGPT, Quivr, Verba and Morphik were also audited. Two
projects left the field this year and are worth knowing about because they were
the closest things to Marrow's pitch: **Hyperlink by Nexa AI** — `nexa.ai` now
301s to `aihub.qualcomm.com`, the product page is gone, the successor
[qualcomm/GenieX](https://github.com/qualcomm/GenieX) is an inference runtime,
not a document tool — and **screenpipe**, which relicensed from MIT to a
bespoke commercial licence on 2026-06-10 while continuing to be listed as open
source nearly everywhere.

## 3.2 Local model runners

| Project | ★ | Lang | Licence | Last activity |
|---|---|---|---|---|
| [Ollama](https://github.com/ollama/ollama) | 179,765 | Go | MIT | 2026-08-29, v0.33.2 |
| [llama.cpp](https://github.com/ggml-org/llama.cpp) | 126,314 | C++ | MIT | 2026-08-30 |
| [Open WebUI](https://github.com/open-webui/open-webui) | 150,391 | Python/TS | **not OSI** (BSD-3 + branding clause) | 2026-08-29 |
| [GPT4All](https://github.com/nomic-ai/gpt4all) | 77,392 | C++ | MIT | **2025-05-27 — dormant** |
| [LocalAI](https://github.com/mudler/LocalAI) | 48,757 | Go | MIT | 2026-08-30 |
| [Jan](https://github.com/menloresearch/jan) | 44,258 | TS | Apache-2.0 | 2026-08-29 |
| [omlx](https://github.com/jundot/omlx) | 20,988 | Python | Apache-2.0 | 2026-08-29 |
| [mlxcel](https://github.com/lablup/mlxcel) | 383 | **Rust** | Apache-2.0 | 2026-08-30 |
| [rMLX](https://github.com/Pushkinist/rMLX) | 12 | **Rust** | Apache-2.0/MIT | 2026-08-24 |

## 3.3 Agent CLIs — **Verified**, 2026-08-30

| Project | ★ | Lang | Licence | Pushed |
|---|---|---|---|---|
| [openai/codex](https://github.com/openai/codex) | 119,947 | Rust | Apache-2.0 | 2026-08-30 |
| [OpenHands](https://github.com/OpenHands/OpenHands) | 85,608 | TS | MIT | 2026-08-30 |
| [cline](https://github.com/cline/cline) | 67,154 | TS | Apache-2.0 | 2026-08-30 |
| [goose](https://github.com/aaif-goose/goose) | 53,681 | Rust | Apache-2.0 | 2026-08-29 |
| [aider](https://github.com/Aider-AI/aider) | 48,599 | Python | Apache-2.0 | **2026-05-22** |
| [continue](https://github.com/continuedev/continue) | 35,697 | TS | Apache-2.0 | 2026-08-30 |

Two things to note. `block/goose` now redirects to `aaif-goose/goose` — the org
moved. And Aider has not been pushed in three months, which for the tool most
often cited as the reference AI pair-programmer is worth knowing.

---

# 4. Provenance

Marrow's claim: every IR node carries a `source_span`, and a citation resolves
to a page, a cell or a line.

**Where the field actually lands.** Of nineteen document-Q&A projects audited,
exactly two resolve below a chunk:

| Resolves to | Who |
|---|---|
| Whole document | Khoj, Quivr, Open Notebook, AnythingLLM, Hyperlink |
| Chunk id | Onyx, PrivateGPT, cognee, Reor, LEANN, SurfSense |
| Page ordinal | Morphik |
| **Page + bbox / sheet + cell** | **RAGFlow** |
| **Page + verified character span** | **kotaemon** |

Onyx is the sharpest illustration, because it has the data and throws it away.
Its `SearchDoc` carries `chunk_ind` and Vespa `match_highlights`; its
`CitationInfo` is `{citation_number, document_id}` and the citation processor
renders `[[n]](link)`. The chunk index never leaves the retrieval layer.
*(Reported.)*

**So Marrow's schema is ahead of almost everyone — and its parsers are not.**
`SourceSpan::Page` is annotated *"Deferred (M0 F3)"* in `crates/core/src/model.rs`;
`Cells` has no producer; PDF is dropped. What exists today is `Bytes` and
`Lines`, which is better than a chunk id and is not "page, cell or line".
RAGFlow ships `[page,l,r,t,b]` and `(sheet,r1,r2,c1,c2)` today.

**The counterargument worth engaging.** arXiv **2604.01432** (Apr 2026) finds
that *enforcing* sentence-level citation degrades attribution quality by 16–276%
versus paragraph level, and the penalty grows with model size. That result is
about making a model *generate* fine-grained citations. Marrow's design is the
other regime — the index supplies the span, the model picks among pre-chunked
units — so it is not directly hit. But a doc that claims finer is better should
say that the literature does not agree unconditionally. *(Reported.)*

**What nobody does, including Marrow: verify the citation.** Anthropic's
Citations API is the only production system that extracts `cited_text`
mechanically, so a pointer cannot dangle — and even that guarantees pointer
validity, not support. kotaemon gets closest in open source (§12.1).
`crates/desktop/src/ask.rs` renders the retrieval result as the citation list;
nothing checks that an `[E1]` in the answer names a real block, or that the
claim it decorates is supported.

---

# 5. Untrusted content and prompt injection

Marrow's mechanism, in `crates/model/src/envelope.rs`: a per-session
unpredictable delimiter, regenerated on collision rather than escaped, with
untrusted content structurally never last and a runtime instruction closing the
prompt.

**This is Marrow's clearest lead, and the margin is embarrassing.** Across
thirteen audited RAG repos, not one treats retrieved document content as
untrusted. Ranked best to worst *(Reported)*:

| | |
|---|---|
| kotaemon | context as a separate `HumanMessage` — structural, but unlabelled |
| RAGFlow | `<context>` tags with ids |
| Onyx | JSON inside a tool result |
| SurfSense | `<document>` XML — **attributes escaped, passage content not**, so a chunk containing `</document>` closes the delimiter |
| AnythingLLM | `[CONTEXT n]` … `[END CONTEXT n]`, unescaped, **inside the system prompt**, duplicated across 38 provider files |
| PrivateGPT, Morphik, Khoj, Verba, LEANN, Reor | bare concatenation |
| **Quivr, Open Notebook** | interpolated into a system prompt template with no delimiters at all |

The only genuine trust label found anywhere is cognee's
`"Prior assistant (untrusted retrieval guidance): …"`, with a named test — and
it guards prior assistant turns, not file content.

The coding agents are no better. **Verified:** Goose's security scanner is
**off by default** — `crates/goose/src/security/mod.rs` reads
`SECURITY_PROMPT_ENABLED`, `SECURITY_PROMPT_CLASSIFIER_ENABLED` and
`SECURITY_COMMAND_CLASSIFIER_ENABLED` and `unwrap_or(false)` on all three — and
`scanner.rs` inspects `CallToolRequestParams`, i.e. tool *requests*, not tool
*results*. Neither Goose nor Codex puts a trust boundary in the agent's prompt.

**A bug class worth naming: forgeable citations.** Morphik appends
`Source: [file.pdf, page 3]` as plain text inside the same untrusted block;
SurfSense inserts content unescaped inside `<document>`; AnythingLLM does not
escape `[END CONTEXT n]`. In all three, a malicious document can fabricate its
own provenance marker. Out-of-band structure prevents this by construction,
which is exactly what Marrow's regenerating delimiter buys.

**And Marrow is honest about the limit,** which most of the field is not. Part 8
§139.5 TIER-025 records that the pinned Qwen 3 0.6B complies with a direct
override embedded in an `EVIDENCE` block. The envelope is defence in depth; the
document says so and then declines to give that model a tool. That is the right
posture, written down.

---

# 6. Self-citation

Marrow marks agent-written files `origin = SELF` and drops them from evidence,
surfacing the omission in `Envelope::excluded`.

**Nobody else models this. The failure it prevents was measured eight days ago.**

- **arXiv 2608.22118** (2026-08-22), *"RAG Collapse: LLM Responses Collapse When
  Retrieved Documents Are Self-Authored"* — 1,528 simulations, **79.6% end in
  collapse**; *"even a single self-authored reference can trigger collapse
  because the LLM disproportionately cites its own content."* The abstract
  proposes **no mitigation**.
- **arXiv 2602.16136** (WWW '26) defines pool/exposure/citation contamination
  rates and calls for "retrieval-aware strategies" without naming a mechanism.
- Mechanistic backing: **arXiv 2404.13076** (NeurIPS 2024) — self-recognition
  capability correlates linearly with self-preference.

Grepping every audited repo for `is_ai_generated`, `generated_by`,
`self_authored`, `origin = SELF`, `machine_generated` returned **zero hits**.
Two projects are worse than silent: Open Notebook prompts the model to cite its
own generated `insight:` records alongside real sources, and SurfSense has the
discriminator (`DocumentType.ARTIFACT`) and uses it to *advertise* generated
artifacts as searchable evidence.

Nor is it recommended anywhere. The OWASP RAG Security Cheat Sheet asks for
provenance tracking — who uploaded it, when, from where — and says nothing about
model-generated documents. C2PA has `c2pa.trainedAlgorithmicData`; W3C PROV-O
models lineage; neither is wired to any retrieval system. Kagi's SlopStop is the
only production system acting on AI-origin at all, and it *downranks* via
community reports rather than excluding, in web search rather than a private
corpus. *(Reported, with the two arXiv IDs verifiable directly.)*

The honest framing: **the failure mode is now empirically established, the fix
is obvious in hindsight, and nobody has shipped it.** That is a real gap. It is
also, in Marrow, half-built — see §13.3.

---

# 7. Write-tool safety — **Verified**

Marrow's `crates/tools/src/guard.rs` is a nine-step path: name rules →
containment by component-wise canonicalisation → protected/excluded subtrees →
case and NFC/NFD collision → temp file `O_EXCL` in the *destination directory* →
re-canonicalise the temp file → **stale digest check** → **atomic rename** →
`Origin::SelfWritten` in the return type with no constructor that produces
anything else.

Here is what the two leading Rust coding agents do. All four claims below were
read from raw source on 2026-08-30.

**Goose** — `crates/goose/src/agents/platform_extensions/developer/edit.rs`:

```rust
// file_write, line 91
match fs::write(path, &params.content) { … }

// file_edit: read at 118, write at 134
let content = match fs::read_to_string(&path) { … };
match fs::write(&path, &new_content) { … }
```

Truncate in place. No temp file, no rename, and **no precondition that the file
is unchanged between the read at 118 and the write at 134**. And the default
posture is permissive — `crates/goose-provider-types/src/goose_mode.rs`:

```rust
pub enum GooseMode {
    #[default]
    #[strum(message = "Automatically approve tool calls")]
    Auto,
    …
}
```

**Aider** — `aider/io.py::write_text` is `open(str(filename), "w")` with
exponential backoff on `PermissionError`. Truncate in place, no stale check.
Its path handling is not naive — `utils.safe_abs_path` calls `Path(res).resolve()`,
which does follow symlinks — but `base_coder.allowed_to_edit` checks gitignore
and prompts "Create new file?" and contains **no containment check against the
repo root**. The gate is a confirmation prompt and git, not the filesystem.

**Codex** is the most serious of the three, and its own comments are the best
description of its limits. `codex-rs/core/src/safety.rs`:

```rust
// Even though the patch appears to be constrained to writable paths, it is
// possible that paths in the patch are hard links to files outside the
// writable roots, so we should still run `apply_patch` in a sandbox in that case.
…
// Only auto-approve when we can actually enforce a sandbox. Otherwise
// fall back to asking the user because the patch may touch arbitrary paths…
```

So Codex's in-process check is **lexical**, it knows it, and it refuses to
auto-approve when no OS sandbox is available. The real boundary is the sandbox —
and on Linux that is **bubblewrap plus seccomp, not Landlock**, verbatim from
`codex-rs/linux-sandbox/src/landlock.rs`:

> *"Note: this is currently unused because filesystem sandboxing is performed
> via bubblewrap. It is kept for reference and potential fallback use."*

I could **not** locate Codex's production write syscall — the `fs::write` hits
in `apply-patch` are all test setup — so I make no claim about whether Codex
writes atomically. Treat that cell as unknown.

**The comparison, stated plainly.**

| | Containment | Stale check | Atomic write |
|---|---|---|---|
| Marrow | canonicalise **at operation time**, re-verified after the temp file exists | **yes**, digest immediately before rename | temp in destination dir + `rename` |
| Codex | lexical + OS sandbox (the real boundary) | not found | **unknown** |
| Goose | `resolve_path`, no containment found | **no** | no — `fs::write` |
| Aider | resolves symlinks, no root containment | **no** | no — `open(…, "w")` |

Marrow is genuinely better *at the write primitive*, and far less ambitious in
scope — no shell, no sandbox, no arbitrary command execution. Those are not the
same claim, and the doc should not let one borrow credit from the other.

**The adversarial corpus is the more unusual asset.** 59 cases as TOML data
rather than test functions, a `the_corpus_only_ever_grows` floor test, and TOCTOU
cases that were *mutation-tested* — disabling the pre-write re-canonicalisation
turns a case red. Codex has ~100 seatbelt test functions and named cases like
`sandbox_blocks_codex_symlink_replacement_attack`, which is the closest
equivalent in the cohort, but those test policy correctness rather than
malicious inputs. **No surveyed project maintains a growing corpus of malicious
inputs as a shipping gate for writes.**

Two caveats, honestly: the corpus covers only the write path — hostile PDFs,
zip-slip, decompression bombs, OCR and EXIF injection are listed in the TRACKER
and are absent, which `corpus/adversarial/README.md` states itself. And a
repo-wide *negative* claim about Goose is weaker than the equivalent claim about
Codex, because GitHub's code-search index returns nothing for `aaif-goose/goose`;
the Goose findings above rest on direct file reads, which is fine for the
positive claims made.

---

# 8. Egress

Part 9's policy: HTTPS only, port 443 only, decide on the **resolved IP**,
re-check every redirect hop, never persist the allow-list.

**Verified in `crates/net`:** `Https` pins the checked addresses into a custom
`ureq` `Resolver` (`Pinned`), so the name is never resolved a second time. That
is not a detail — it is the difference between a policy and a decoration.

**Because "resolve, validate, then connect" is the exact shape of at least three
2026 CVEs, every one of them a *second* advisory on code that already had an
SSRF check** *(Reported, IDs verifiable)*:

| | |
|---|---|
| **CVE-2026-27826** (`mcp-atlassian`) | the prior fix resolved, checked each IP is global, **discarded the IP**, then re-resolved the hostname. Recommended remediation: *"Pin the connection to the IP that `validate_url_for_ssrf` validated."* |
| **CVE-2026-41488** (LangChain) | their own February fix, rebound two months later |
| **CVE-2026-27127** (Craft CMS) | *"performs DNS resolution separately from the HTTP request"* — also a bypass of an earlier fix |

Open WebUI carries **fourteen** CWE-918 advisories, and the arc is the whole
lesson: no check → a `validators` blocklist → an `ipaddress` blocklist →
redirect revalidation → sub-resource coverage → resolved-IP pinning at the
connection layer in 0.11.0. Nine months, ten advisories, to arrive where
`crates/net` started. One of those, **CVE-2026-70485** (HIGH, 2026-08-02), is
NAT64: `http://[64:ff9b::a9fe:a9fe]/latest/meta-data/` returned 200 because
`is_global` returns **True** for that prefix by design. Marrow's `unwrap_v4`
handles mapped, v4-compatible and `64:ff9b::/96` and classifies by the embedded
IPv4 — the case that cost Open WebUI a HIGH.

**Where Marrow is ahead of the entire surveyed field:**

- **HTTPS-only.** Nobody does this. Open WebUI, Onyx, LangChain and the
  reference `mcp-server-fetch` all permit plain `http`.
- **Consent never persisted.** Nobody does this. Everyone persists config, which
  is how "an exception added once, forgotten" becomes permanent.
- **Every redirect hop re-checked.** This is the gap behind GHSA-5x7x-4c3c-qf5w
  (Open WebUI, 2026-08-29) and CVE-2026-41481 (LangChain), and it is still open
  in Onyx's web connector.
- **CGNAT `100.64.0.0/10` refused** — absent from n8n's defaults, and Part 9 is
  explicit that this breaks Tailscale hosts on purpose.

For calibration on how low the bar is: the reference **`mcp-server-fetch`** has
no egress control whatsoever — grepping the shipped wheel for
`ipaddress|is_global|127\.0\.0\.1|169\.254|getaddrinfo` returns zero matches, and
the README says so. LangChain's good SSRF module lives in `langchain-core` and
`grep validate_safe_url` in `langchain-community` — the package a RAG app
actually fetches with — returns nothing. Onyx contains a correct
`_PinnedHostAdapter` and did not apply it to its web connector. *(Reported.)*

**Three things to add anyway** — see §12.3.

---

# 9. Model lifecycle

This is the section where Marrow is least differentiated, and the research is
unambiguous about it. *(All Reported.)*

| Mechanism | Who already has it |
|---|---|
| Admission against a **live** OS free-memory sampler | **Ollama** (`host_statistics64`, 80%-of-free threshold, pre-flight in `server/sched.go`) and **omlx** (`host_statistics64`, ceiling re-polled every cycle) |
| Memory watchdog evicting under pressure | **omlx only** — soft/hard watermarks → LRU evict **and pause admission** |
| Idle eviction | Commodity. Ollama 5m, LM Studio 60m TTL, llama.cpp `--sleep-idle-seconds`, LocalAI 15m, rMLX 15m |
| KV prefix reuse across requests | Commodity. Ollama ships a compressed prefix **trie** shared across conversations; llama.cpp has `--cache-reuse`, slots and checkpoints; `mlx_lm` itself has `PromptTrie` + `LRUPromptCache` |
| **Circuit breaker on repeated load failures** | **Nobody.** Ollama has a one-shot `oomRetryAttempted`; omlx a sticky `load_failed` until refresh; LocalAI retries on a fixed interval forever |

`jundot/omlx` (20,988★, Apache-2.0, created Feb 2026) is close to a line-by-line
match for Part 8's supervisor: live sampler, a dynamic ceiling that moves as
other apps come and go, watermark-driven LRU eviction *with admission pause*,
per-model TTL, and a block-hashed prefix cache with hot-RAM and cold-SSD tiers
surviving restart. Its weakness is the one Marrow can attack: it pins
`mlx==0.32.0` and an exact `mlx-lm` git SHA because of ABI-coupled kernels.

**The persisted per-model circuit breaker is the one genuinely unmatched piece,**
and Part 8 §142.4's reasoning for it — *"three failures in a row is information"* —
is better than anything in the field. Everything else in Part 8 is table stakes
executed carefully.

**And the Rust/MLX story is not what the README implies.** Marrow's "MLX
runtime" is `crates/model/worker/mlx_worker.py`, 376 lines of Python driving
`mlx_lm`, launched from a venv the user must create by hand
(`python3.11 -m venv`, `pip install mlx-lm`). Meanwhile:

- **Ollama** wrote its own Go CGo bindings to the MLX C API, with hand-written
  Metal kernels. Not Python.
- **mlxcel** (383★, Rust, Apache-2.0, commits landing the hour it was checked)
  serves models *"through native MLX C++ bindings"*, with continuous batching,
  prefix caching on by default, and llama-server-compatible flags.
- **rMLX** (12★, Rust) is *"bindgen wrapper around brew-prebuilt mlx-c"* — the
  cheap path — with per-model TTL and block-hashed prompt caching.
- **`mlx-rs` is not a foundation.** The org renamed to `oxiglade/mlx-rs`; `main`'s
  HEAD is 2026-03-27; crates.io's latest is 0.25.3 from 2025-12-16; it pins
  MLX-C 0.4.0 against a current 0.6.0.

Three independent teams routed around `mlx-rs` and wrote their own FFI. That is
the signal: the bindings are real work, and shelling to Python is the option
none of them took.

Where Marrow *is* ahead of mlxcel: mlxcel's `--estimate-memory` preflight uses
`sysctl hw.memsize` — **total** physical RAM, cached — on Apple Silicon. On the
exact platform in question its admission is a static estimate against a
constant. Marrow's live sampler fills that gap. mlxcel is the project to
benchmark against; omlx is the project to read.

---

# 10. Local-first honesty

| Project | Runs with no network and no account? |
|---|---|
| **Marrow — `search`** | **Yes.** No model, no key, no service |
| **Marrow — `ask`** | Yes *after* a manual Python venv and `pip install mlx-lm` — which needs a network once and is not in the README's stack table |
| AnythingLLM desktop | Yes — bundled MiniLM + LanceDB, telemetry opt-out |
| Khoj | Yes — `USE_EMBEDDED_DB=true --anonymous-mode`, vendored Postgres |
| cognee | Yes for search; `cognify()` ingestion always needs an LLM |
| Onyx | Yes, but ten containers, and "lite" disables RAG |
| RAGFlow | Yes — 16 GB RAM, 50 GB disk |
| SurfSense | ~11 services |

None of them require a vendor account to self-host. The differentiator is not
"local" — that is table stakes in this cohort — it is *how little* has to be
standing up for the core function to work.

**Search-without-an-LLM is where the field actually splits.** Khoj's bi-encoder
is mandatory; Morphik's FTS is filename-only; AnythingLLM is vector-only; LEANN
is vector by construction. Worst of all is Reor, which ships a keyword slider
that calls the *vector* search and re-scores the results with regex word counts —
a lexical fallback that can never surface a document the embedder missed, and
fails invisibly. Marrow's hard rule #10 is a real differentiator against half
this list. It also has no test (§12.2).

---

# 11. Where Marrow is genuinely ahead

Five things, in descending order of confidence.

1. **The evidence envelope.** Nothing in the surveyed field labels retrieved file
   content as untrusted with out-of-band structure. The regenerating delimiter,
   the never-last rule and the closing runtime instruction are all mechanisms,
   not prose, and each has a named test.
2. **Egress.** HTTPS-only and non-persisted consent are unique. Address-pinned
   connection and per-hop revalidation are what three 2026 CVEs were *missing*.
   The NAT64 unwrapping is the specific case that cost Open WebUI a HIGH.
3. **The write primitive.** Re-canonicalisation at operation time, a stale digest
   check with nothing between it and the rename, and an atomic rename — against
   `fs::write` and `open(…, "w")` in the two leading Rust and Python agents,
   neither of which checks the file is unchanged.
4. **The adversarial corpus as data with a monotonic floor**, and TOCTOU cases
   proven by mutation rather than assertion.
5. **`origin = SELF`.** Unimplemented across the entire competitive set, and the
   failure it prevents was measured at a 79.6% collapse rate in August 2026 —
   with no mitigation proposed by the paper that measured it.

---

# 12. Where Marrow is behind, or rebuilding

## 12.1 The model runtime is a rebuild, and a compromised one

§9 is the case. Ollama and omlx got to live-sampler admission first; idle
eviction and prefix reuse are commodity; and the implementation is a Python
sidecar where three other teams wrote bindings. The persisted circuit breaker is
worth keeping. The rest of Part 8 is 8,595 lines of `crates/model` spent
competing with projects that do it better, on the milestone *after* the one
whose exit criterion ("use it for a week") is still unticked.

## 12.2 Libraries to adopt instead of writing

The single highest-leverage item first.

| Adopt | Instead of | Why |
|---|---|---|
| **`objc2-pdf-kit`** | pdfium | `PDFPage::characterBoundsAtIndex` gives a per-character `CGRect` in page space, and `numberOfTextRanges(on:)` / `range(at:on:)` map a selection back to `NSRange`s within that page's string. That is **page + char range + bbox, free from the OS** — and it removes a multi-megabyte Chromium dylib to bundle, sign, notarize and version-track. It also un-deprecates `SourceSpan::Page`, currently dead code |
| **`objc2-vision`** | `ocrs`, any Tesseract binding | `VNRecognizeTextRequest`: 30 languages accurate / 6 fast, ~51–67 ms warm, per-range bounding boxes, on-device, no entitlement, no model download. Every Rust Tesseract binding is dead — `leptess` (2023), `rusty-tesseract` (shells out), `extractous` (2024) |
| **`text-splitter` 0.32.0** | the hand-written chunker | It returns chunks *with their byte and character offsets*, and is Unicode-aware by construction. The TRACKER log records a char-boundary panic in the chunker on a box-drawing glyph — precisely the bug this crate exists not to have |
| **`objc2-natural-language`** | hand-rolled sentence splitting | `NLTokenizer(unit: .sentence)` splits "Dr. Smith went to Washington. He arrived at 5 p.m." into 3, not 5. **Not `NLEmbedding`** — three languages, and it scores 0.988 distance between paraphrases |
| **`setiopolicy_np(IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES, …, OFF)`** | discipline | One libc call turns hard rule #3 from a check the code must remember into **kernel policy**, inherited by child processes: the kernel fails the open instead of downloading. `SF_DATALESS` detection is correct and is not enforcement |

Two configuration bugs the research surfaced that are worth a test each:

- **FTS5's default token categories are `"L* N* Co"` — `Mn` is excluded**, so NFD
  text splits at every combining accent. Marrow handles NFC/NFD in path keys and
  in `guard.rs`; the *index* tokenizer has the same bug with no test. Fix:
  `tokenize = "unicode61 categories 'L* N* Co Mn'"`.
- **`notify` cannot reach the FSEvents flags this design needs.** `sinceWhen` and
  `kFSEventStreamCreateFlagFullHistory` are how you resume after `kill -9`
  without silently skipping events (hard rule #7); `UseExtendedData` +
  `fileID` is the **only** way to pair a rename (hard rule #2) — FSEvents
  delivers the old and new paths as two unlinked events. Also, `notify` 8.2.0
  predates macOS panic fixes that exist only on the 9.0.0-rc line.

## 12.3 Egress: three additions

1. **Add `0.0.0.0/8` explicitly.** On macOS `0.0.0.0` routes to localhost, and
   Ollama binds `0.0.0.0:11434` — a service Marrow itself detects.
2. **Add the named globally-routable addresses** from Open WebUI's
   `DEFAULT_WEB_FETCH_FILTER_LIST`: `168.63.129.16` (Azure platform channel,
   reachable from every Azure VM), `100.100.100.200` (Alibaba),
   `192.88.99.0/24` (6to4 relay anycast), `2001:1::1`/`::2`, `2001:20::/28`,
   `5f00::/16`. Category classification is the right design and these are
   addresses it will pass.
3. **Reject parser-differential characters** — `\`, tab, CR, LF in a URL — as
   Open WebUI now does. CVE-2026-45400 was `urlparse` reading host `1.1.1.1`
   from `http://127.0.0.1:6666\@1.1.1.1` while `requests` connected to loopback.

## 12.4 Citation verification does not exist

§4's last paragraph. This is the largest functional gap against the flagship
claim, and §12.5.1 is the cheapest fix in this document.

## 12.5 Ideas worth stealing, concretely

**1. kotaemon's quote-then-locate.** `libs/kotaemon/kotaemon/indices/qa/citation.py`
makes the model emit *verbatim quotes* (`"a direct quote from the context, as a
substring of the original content (max 15 words)"`, with `tool_choice: required`)
rather than indices. `qa/utils.py::find_text()` then relocates each quote by
`SequenceMatcher(...).get_matching_blocks()` and **rejects the span** unless the
merged match exceeds `max(len(sentence) * 0.35, min_length)`.

Why Marrow should copy it: it converts an unverifiable `[E1]` into a checkable
character span and **fails closed on a hallucinated citation**. And it composes
exactly with `source_span` — locate the quote's offsets inside a chunk, then map
through that chunk's own span to a line, a cell or a page. It needs no model, no
network, and no new dependency.

**2. Onyx's airgap CI job.** `networks: default: internal: true` plus an
assertion that search still works. Hard rule #10 currently has no test; this
makes it a permanently green proof for near-zero effort, and it is the rule most
likely to rot silently as the model runtime grows.

**3. cognee's answer-grounded evidence filter** (`utils/references.py`) — drop
cited candidates that share no significant terms with the answer actually
produced, and return `""` rather than *"presenting unverifiable retrieval order
as provenance"*. LLM-free and unit-testable. Marrow currently shows the
retrieval result as the citation list, which is exactly the thing cognee refuses
to do.

**4. Docling's `DocMeta.doc_items`** — provenance survives chunking because the
chunk carries the actual IR items, and that provenance is excluded from the
embedded text. Marrow's `merge_spans` falls back to the first span when two
spans cannot be unioned; carrying the item list instead loses nothing. *(Note
the trap: Docling's `charspan` is a decoy — every construction site passes
`(0,0)` or `(0, len(text))`. The usable span is `(page_no, bbox)`. And DOCX
items ship with `prov = []`.)*

**5. AnythingLLM's `cannonball()`** — when a prompt is over budget, excise from
the **middle** and splice in a visible `--prompt truncated for brevity--`
marker. Dropped evidence leaves a trace. Marrow surfaces `excluded` for SELF
content and has no token budget at all (§13.4).

**6. Goose's `git_command()` and its test.** It sets
`-c safe.bareRepository=explicit -c core.fsmonitor=false`, and
`tests/git_command_security.rs` **first proves the hook fires under plain `git`**,
then proves it does not under the hardened command. That is the right shape for
the adversarial corpus's cloned-repo cases: demonstrate the attack works before
demonstrating the defence.

**7. LM Studio's `lms load --estimate-only`** — print the memory arithmetic and
exit. Marrow's Models page shows its working; a CLI verb that does the same is
the version that goes in a bug report.

**8. LEANN's selective recomputation** (MLSys 2026 best paper,
[arXiv 2506.08276](https://arxiv.org/abs/2506.08276)) — store the pruned HNSW
graph but **not** the embeddings, recomputing only for nodes on the search path;
97% storage reduction claimed at no accuracy cost. Relevant to D1's
"brute-force cosine, indefinitely" when the corpus stops being 35k files.

---

# 13. Where the claims are thinner than they sound

Read against the code, not the docs.

**13.1 "Citations to the exact page, cell or line."** Only `Bytes` and `Lines`
have producers. `Page` is marked *"Deferred (M0 F3)"*, `Cells` has no writer,
PDF is dropped (D4), XLSX is unbuilt. The README's own example —
`contract.pdf p17, ¶Renewal` — cannot happen today. The schema carries the
claim; the parsers do not. §12.2's first row is the cheapest route to making it
true.

**13.2 Hard rule #1 and the tracker disagree.** `IrNode.span` is `SourceSpan`,
not `Option<SourceSpan>`, which is genuinely the right enforcement. But TRACKER
M1 still shows `[ ] Parser trait + versioned IR with source_span on every node ⚠️`
unticked while `crates/parse` is 5,948 lines and shipping chunks into the index.
One of the two is wrong and it should be resolved rather than left ambiguous.

**13.3 The self-citation rule is half-closed, and the TRACKER says so.**
`Written::origin()` returns `SelfWritten` and nothing persists it. Until the
write path records `origin = 'SELF'`, the next scan reclassifies agent output as
the user's own and it becomes citable. The envelope's exclusion is correct code
that currently never fires on real data — which makes §11's strongest
differentiator, today, a unit test.

**13.4 The envelope does no budgeting.** Part 6 §114.3 specifies nine ordered
stages: classification drop, dedupe by `text_hash`, max chunks per file, minimum
distinct sources, heading expansion, graph cap, token trim, a secret/DLP scan on
the *assembled* envelope, and the disclosure. `Builder::finish` does the SELF
drop and the delimiter; `ask.rs` caps chunks at 12. There is no token
accounting, no dedup, and no DLP scan. The byte count in `Disclosure` is honest
about what it measures and is not a budget.

**13.5 A residual window in the write path.** `check_precondition` digests, then
`rename`. A write landing between those two is clobbered, and `Expect::New`
cannot fail on a file created in that window because `rename(2)` overwrites
unconditionally. The module comment — *"the last three syscalls are: digest,
rename, done"* — is accurate and is still a window. macOS has
`renamex_np(…, RENAME_EXCL)`, which would close the `New` case by syscall rather
than by check.

**13.6 Part 9's coverage table is exemplary and the README does not carry it.**
§161.1 marks NET-032 and NET-052 *"implemented, untested"* and NET-034's wall
clock as having no test that drives an overrun. Listing that is exactly right.
But the summary framing elsewhere reads as though the policy is fully enforced.

**13.7 "One binary plus a parser subprocess" (§130.3) is now three runtimes** —
a Rust binary, a pnpm/`node_modules` frontend, and a Python venv for inference.

**13.8 The delimiter's strength is unstated.** It is the last 8 Crockford-base32
characters of a ULID: 40 bits, stable for a session. That is sound against an
author who cannot observe it — which is the stated threat model, and the code
comment argues it correctly — but "unpredictable" should carry the number.

**13.9 The scope reversal is the real finding.** Part 7 §130 and `D-agent-layer`
say: no chat UI, no agent runtime, **no model gateway** — MCP is the interface,
and this "deletes roughly 60% of the spec". What exists is a Tauri + React
desktop app, a `GenerationProvider` gateway, a supervisor, and a streaming
conversational Ask surface with turns and history. D42 was explicitly reversed
with reasons; `D-agent-layer` was not, and Part 8 §148's ask pipeline is a model
gateway under another name. Meanwhile M2's exit criterion ("use it for a week")
is unticked, S7 and S8 are both built-but-not-wired (*"MCP wiring left"*), and
the parser IR — hard rule #1 — is an open checkbox.

That is **S1**, the risk Part 7 §134.1 rates *very high / severe*: "scope
collapse into an unfinished everything." It is not a hypothetical in the risk
table any more.

---

# 14. Is the differentiation real?

**Yes, narrowly — and not where the README puts it.**

The retrieval story is not differentiated. Onyx, RAGFlow and kotaemon do hybrid
retrieval better today, and RAGFlow already ships page+bbox and sheet+cell
citations that Marrow's schema describes and its parsers do not produce. On
search quality alone there is no reason for this to exist.

What is differentiated is the **policy layer**, and it is differentiated by an
uncomfortable margin:

- Nobody labels retrieved file content as untrusted with out-of-band structure.
- Nobody models "the assistant wrote this", eight days after a paper measured a
  79.6% failure rate from exactly that.
- Nobody ships HTTPS-only, non-persisted, per-hop-revalidated, address-pinned
  egress — and the projects that tried the easier version have the CVEs.
- The two leading coding agents overwrite user files in place without checking
  whether the file changed since they read it.

The honest one-sentence justification: **it is the only thing in the field where
a local index, a local model and a write tool sit behind a single policy boundary
that treats file content as data.** Khoj and Onyx are better search products and
will stay that way. Neither will ever be safe to point at a write-capable agent,
because neither models the question. For one person who intends to wire this to
Claude Code, that is a real reason for it to exist.

**The risk is not that the differentiation is fake. It is that the differentiated
parts are the unfinished ones.** S7 and S8 — the write guard and the egress
policy, the two strongest results in this document — are both marked "MCP wiring
left", which means neither is reachable from the agent they exist to protect.
Meanwhile 8,595 lines went into a model runtime that Ollama, omlx and mlxcel do
better.

If this comparison suggests one thing to do next, it is: **wire S7 and S8 to
MCP, persist `origin = SELF`, and add the airgap test** — three items that turn
the strongest claims in this document from correct code into working defences.

---

# 15. What could not be verified

Stated rather than smoothed over.

- **Codex's production write syscall.** The `fs::write` calls in `apply-patch`
  are test setup; I did not locate the real one. No claim is made about whether
  Codex writes atomically.
- **Cline, OpenHands and Continue** appear in §3.3 with metadata only. Their
  path-safety and stale-check behaviour was not verified and no claim is made
  about them.
- **Goose repo-wide negatives** are weaker than the Codex equivalents:
  GitHub's code-search index returns nothing for `aaif-goose/goose`. The Goose
  claims made here are all positive claims read from specific files.
- **Marrow's own `cargo test` was not run** — another process holds the tree.
  Test counts are from the TRACKER and from reading the test modules.
- Everything in §9 about Ollama's and omlx's internals is **Reported**, not
  re-read. The `mlx-rs` staleness, the mlxcel `hw.memsize` finding and the
  Ollama prefix trie are the load-bearing ones and are worth re-checking before
  acting on §12.1.
- Anthropic's Citations API launch date is unreconciled between sources
  (Jan vs Jun 2025). Its accuracy figures are vendor-internal and unpublished.
- Several vendor pages (Perplexity, OpenAI help centre, Adobe) returned 403 or
  timed out; product-level claims about their citation UIs rest on developer
  docs and search snippets, not retrieved pages.
- **CVE-2024-37032 "Probllama" is CWE-22 path traversal to RCE, not SSRF**, and
  Ollama has no SSRF CVE — corrections to a premise this comparison started
  from, recorded so the error is not repeated.
