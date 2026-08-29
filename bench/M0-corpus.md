# M0 — Corpus measurement

**Date:** 2026-08-30
**Machine:** Mac16,12 (Apple Silicon), 10 cores, 16 GB unified memory, macOS 26.3
**Disk:** 228 GB volume. **17 GB free at measurement time; 78 GB free after reclaiming 64 GB of Rust `target/` dirs (2026-08-30).** See F11.
**Method:** `spike/` — `ignore` walk + blake3 + rusqlite, release build. Aggregates only; no filenames recorded.

> **Purpose:** replace the specification's assumptions with facts. Several of them were wrong by an order of magnitude.

---

## 1. Scale

| Stage | Files | Reduction |
|---|---|---|
| Raw `find` over candidate roots | **872,826** | — |
| After `.gitignore` (`ignore` crate defaults) | 25,092 | −97.1% |
| After `.gitignore` + explicit noise dirs | **9,435** | **−98.9%** |

Noise directories excluded: `node_modules` (15 dirs, 76,097 files on their own), `.git`, `target`, `build`, `dist`, `.venv`, `venv`, `__pycache__`, `.gradle`, `.next`, `vendor`, `Pods`, `DerivedData`.

| Indexable set | |
|---|---|
| Files | **9,435** |
| Directories | 777 |
| Total bytes | **0.99 GB** |
| Symlinks | **0** |
| Walk errors | **0** |
| Duplicate content | 3.6% (9,091 unique of 9,434 hashes) |

**Root distribution (raw, pre-exclusion):** Desktop 872,826 · Pictures 6,647 · Applications 2,173 · Downloads 399 · Music 145 · Movies 90 · Documents 3 · everything else ≤ 3.

Effectively the entire corpus is `~/Desktop`. Documents and Downloads are empty by comparison.

---

## 2. Composition

| ext | files | MB | Class |
|---|---|---|---|
| jpeg | 3,338 | 415.7 | photo |
| dat | 1,748 | 2.7 | app-internal |
| rs | 839 | 15.9 | **code** |
| txt | 449 | 0.1 | **text** |
| toc | 318 | 0.0 | app-internal |
| journal | 318 | 155.6 | app-internal |
| md | 289 | 5.7 | **text** |
| strings | 205 | 0.0 | app-internal |
| tsx | 153 | 1.0 | **code** |
| plist | 145 | 0.2 | app-internal |
| jpg | 140 | 3.2 | photo |
| ts | 118 | 0.7 | **code** |
| toml | 118 | 0.2 | **config** |
| (none) | 97 | 27.8 | mixed |
| sql | 96 | 3.7 | **code** |
| mjs / js | 149 | 8.0 | **code** |
| py | 63 | 0.4 | **code** |
| css | 53 | 1.5 | **code** |
| ttf / woff2 | 86 | 21.0 | font |
| json | 47 | 0.2 | **config** |
| html | 41 | 12.3 | **code** |
| xlsx | 33 | 14.1 | **document** |
| docx | 33 | 19.7 | **document** |

189 distinct extensions total.

### Document and media formats, counted precisely

| Format | Post-gitignore | Raw (pre-gitignore) | Note |
|---|---|---|---|
| **pdf** | ~0 | **14** | Fourteen. In the entire home directory |
| docx | 33 | 41 | |
| xlsx | 33 | **475** | 442 hidden by `.gitignore` — see §5 |
| csv | <33 | 91 | |
| pptx / doc / xls | 0–1 | 2 | |
| **eml / mbox** | 0 | **0** | No email corpus at all |
| **mov / mp4** | 0 | **0** | No video at all |
| **mp3 / wav** | 0 | 3 | No audio corpus |
| png / gif / heic | ~96 | 96 | |
| Screenshots (by name) | — | **13** | |

### Size distribution

| Bucket | Files | Share |
|---|---|---|
| < 64 KB | 6,658 | 70.6% |
| < 1 MB | 2,672 | 28.3% |
| < 50 MB | 104 | 1.1% |
| < 500 MB | 1 | 0.0% |
| ≥ 500 MB | **0** | 0.0% |

---

## 3. Performance

Release build, warm page cache, single run.

| Operation | Measured | Spec target | Verdict |
|---|---|---|---|
| `ignore` walk | **97,308 files/s** (9,435 in 0.10 s) | 100k files < 30 min (§56 P1) | **~1,700× faster than required** |
| blake3 hash | **417 MB/s**, 4,209 files/s | — | 0.91 GB in 2.24 s |
| SQLite batched insert | **234,725 rows/s** | 5–20k rows/s (§50) | **~12–47× the estimate** |
| SQLite `LIKE` scan, 9.4k rows | 1.0 ms | — | |
| DB size | 4.8 MB (530 bytes/row) | — | |
| **Full index pass (walk + hash + store)** | **~2.4 s** | 20–30 min | — |

The entire corpus can be walked, hashed and persisted in under three seconds.

---

## 4. What this changes

| # | Finding | Consequence |
|---|---|---|
| **F1** | Corpus is **9.4k files / 1 GB**, not 100k / 1–5M chunks | Every scale mechanism in the spec is unnecessary: generational vector indexes, vector quantization, disk-backed graph traversal, per-workspace partitioning, the adaptive resource governor. **Do not build them.** |
| **F2** | SQLite writes at 235k rows/s, not 5–20k | The single-writer batching design (§50) is still correct, but backpressure and throughput tuning are non-problems. Ship the simple version |
| **F3** | **14 PDFs in the entire home directory** | PDFium, page+bbox provenance, scanned-PDF detection, OCR and borderless-table reconstruction are ~15 weeks of spec'd work serving 14 files. **Drop PDF from M3.** Revisit only if the corpus changes |
| **F4** | Zero video, zero audio, zero email | `VID` (12), `AUD` (10), `MAIL` (10) — 32 requirements with **no corpus to justify them**. Already deferred in Part 7; now provably dead |
| **F5** | Real content is **code + markdown + config** (~2,000 files) plus **3,478 photos** | M1 parser priority reorders: Tree-sitter (Rust/TS/JS/Python/SQL) → Markdown → text → TOML/JSON → image `META`. Office formats are 66 files, low priority |
| **F6** | 3,478 photos, only 13 screenshots | OCR value is near zero. **EXIF/`META` extraction is the entire image story** — cheapest knowledge in the system (§69.2), and it's 35% of the corpus by count |
| **F7** | 70.6% of files < 64 KB, nothing ≥ 500 MB | The 64 KB chunk-body threshold (§50) means almost everything lives inline in SQLite. Large-file streaming paths are unexercised |
| **F8** | **`.gitignore` does 97% of the exclusion work** | It's the single highest-leverage default. But see F9 |
| **F9** | 442 of 475 `.xlsx` are hidden by `.gitignore` | **Risk.** Respecting gitignore globally makes real data invisible. It must be a **per-root policy**, not a global default (FS-002 says "where configured" — honour that) |
| **F10** | 0 symlinks, 0 walk errors in the current corpus | Path-escape surface is empty *today*. Not a guarantee — one cloned repo changes it. Keep the defence (invariant #7); just don't expect it to fire during development |
| **F11** | ~~17 GB free disk~~ → **78 GB free** | **Superseded same day.** 64 GB was reclaimable Rust `target/` output (40 GB in one project). Disk is no longer a constraint: a 7B Q4 model is ~4.5 GB against 78 GB free. Ollama-if-present is still preferred, but on maintenance grounds, not space. **Real lesson: build artifacts were 6.6× the entire knowledge corpus** — which is exactly why noise exclusion is the highest-leverage default (F8) |
| **F12** | 16 GB unified memory, M4-class | **T-mid** tier (§95.3). 7–8B @ Q4 comfortable, 13–14B tight, 30B+ out of reach |

---

## 5. Decisions this settles or reopens

| Decision | Was | Now |
|---|---|---|
| **D1** — vector store | "Brute force first, LanceDB when it hurts" | **Settled: brute force, probably forever.** 9.4k files ≈ 30–60k chunks. Cosine over 60k × 384 floats is single-digit ms. LanceDB would be a dependency serving nothing |
| **D3** — Tantivy vs SQLite FTS5 | Tantivy | **Reopen.** At 9.4k documents both are instant; FTS5 removes a dependency and a second index to keep consistent. Tantivy's win (tokenizers, field-aware BM25) is real but small at this scale. Lean FTS5, decide at M1 |
| **D4** — PDF engine | PDFium | **Deferred indefinitely.** 14 files (F3) |
| **D5** — platform | undecided | **macOS 26.3, Apple Silicon.** Recorded |
| **D2/D31** — LLM runtime | Candle or Ollama | **Ollama-if-present preferred**, on maintenance grounds. The disk argument evaporated with F11 |
| **New** — gitignore policy | global default | **Per-root policy** (F9) |

---

## 6. Revised M1 scope

**Cut from M1** (no corpus justifies them):
- PDF parsing · OCR · large-file streaming paths · resource governor · generational indexes · vector quantization

**Keep, reordered by actual file counts:**

| Priority | Parser | Files |
|---|---|---|
| 1 | Tree-sitter: Rust, TypeScript/TSX, JavaScript, Python, SQL | ~1,300 |
| 2 | Plain text | 449 |
| 3 | Markdown | 289 |
| 4 | TOML / JSON / YAML | ~165 |
| 5 | Image `META` (EXIF/XMP) — metadata only, no pixels | 3,478 |
| 6 | CSV | ~90 |
| 7 | HTML / CSS | ~94 |
| 8 | XLSX / DOCX | 66 |

**Add to M1's default exclusions** (app-internal noise found here): `*.dat`, `*.toc`, `*.journal`, `*.strings`, `*.plist`, font files. Together ~2,700 files (29% of the indexable set) with no knowledge value. `*.journal` alone is 155 MB.

---

## 7. Unresolved

| Item | Status |
|---|---|
| ~~Cloud placeholder count~~ | **RESOLVED 2026-08-30** during M1 scanner work — see §9. |
| `~/Library` | Never scanned; excluded by design. Caches and app support have no knowledge value and are enormous |
| Corpus volatility | One snapshot. Re-run `spike/` after M1 to see how much the numbers move |

---

## 9. Resolved: the cloud placeholder question

M0 left this open because `find -flags dataless` over `$HOME` timed out at two minutes. The scanner's metadata-only tier check settled it in milliseconds.

| Root | Files | Placeholders | Mechanism | Logical bytes | Time |
|---|---|---|---|---|---|
| `~/Library/Mobile Documents` | 58 | **58 (100%)** | `SF_DATALESS` ×58, `.icloud` stub ×0 | **1.35 GB** | 9–36 ms |
| `~/Library/CloudStorage` | 0 | — | — | — | — |

**The timeout was `find`'s traversal cost, not the flag check.** Reading `st_flags` off an `lstat` we already perform is free.

| # | Finding | Consequence |
|---|---|---|
| **F13** | `.icloud` stubs are effectively extinct here — `SF_DATALESS` fired 58/58 | Keep both mechanisms (older sync clients and non-APFS volumes still produce stubs), but the flag is the live path |
| **F14** | **`hidden(true)` would have hidden every placeholder** | `.icloud` stubs are always dot-prefixed, so the walker's default hidden filter erases the only evidence an evicted file exists. TIER-008's "cloud-only, not indexed" count would have silently read **zero** — a feature that looks like it works. Walk with `hidden(false)` and filter hidden files in our own predicate, with a stub exemption |
| **F15** | **D47 quantified: gitignore off yields 34,459 files on `~/Desktop`, vs 9,435 with it on** | **3.7× larger.** The per-root default is still correct (F9), but **M1 sizing should assume ~34k files, not 9.4k** |
| **F16** | APFS is normalization-**insensitive**, not merely preserving | Both NFC and NFD spellings of one name cannot coexist in a directory; `canonicalize` returns the stored (NFD) form. Normalization must happen in our path key — it cannot be expected from the OS |
| **F17** | `Hydrating` is not observable from metadata | A file mid-hydration still carries `SF_DATALESS`, so it reads as `Placeholder` until complete. `TierState::Hydrating` is never returned by the scanner |
| **F18** | A detached volume surfaces as an `lstat` failure on the **parent**, not as `Unavailable` | Tier detection returns an error rather than a state. Critically, a failed stat never yields "Resident" — the safe direction |

## 8. How to reproduce

```sh
cd spike && cargo build --release
./target/release/marrow-spike ~/Desktop ~/Documents ~/Downloads ~/Pictures ~/Movies ~/Music
```

`spike/` is throwaway. Delete it once M1's real crate layout exists.
