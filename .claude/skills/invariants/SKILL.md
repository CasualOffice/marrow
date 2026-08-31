---
name: invariants
description: The non-negotiable correctness and safety rules for Marrow. Load before writing or reviewing any code that touches files, paths, filesystem watching, parsing, evidence, prompts, or file mutations — these are the rules that are expensive or impossible to fix after the fact.
---

# Marrow invariants

Fifteen rules that survive the solo re-scope, because they protect the author's own files, bandwidth and ability to trust the output. Derived from [Part 7 §126](../../../docs/Part_7_Solo_Rescope.md) and expanded — §126 lists fourteen, this adds the content-hash rule and splits others by subject.

**Test:** if a change violates one of these, it's wrong even if it works.

> **The numbering below is local to this skill.** It is **not** §126's numbering and **not** CLAUDE.md's ten hard rules. `origin = SELF` is **13** here, **10** in §126 and **9** in CLAUDE.md. **Cite these rules by name in code and prose** — "the `origin = SELF` rule", "never hydrate a placeholder". A bare `invariant #N` resolves differently under each of the three lists and is banned; see CLAUDE.md, "How to cite a hard rule".

---

## Identity and provenance

**1. `source_span` on every IR node.**
Page+bbox, sheet+range, XML path, byte range, or timestamp. Provenance is the entire reason this project exists rather than `ripgrep | llm`. Nearly free at write time, nearly impossible to add later.

**2. Path is never identity.**
Files get a stable `file_id`; paths are history (`file_paths`). Never key a cache, a chunk, a vector or an evidence row on a path. Renames must not orphan derived data.

**3. Content hash is identity for dedup, not for files.**
Two files can share a hash legitimately. `blake3` everything; use it for dedup and embedding cache, not for logical identity.

**4. Every derived artifact carries `(source_version, processor_id, processor_version)`.**
This is what makes reprocessing after an upgrade automatic instead of a manual reindex.

---

## Filesystem

**5. Never hydrate cloud placeholders.** ⚠️
Check before *any* read:
- macOS: `SF_DATALESS` flag, `.icloud` stub files
- Windows: `FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS`, `..._RECALL_ON_OPEN`, `..._OFFLINE`
- Linux: known sync-client mount points, by config

Getting this wrong silently downloads the entire cloud drive. Highest-severity finding in the whole spec.

**6. Watchers are hints. Reconciliation is truth.**
`notify` events are lossy on every platform. Never assume "no event == no change". Reconcile periodically and surface degraded watcher state rather than hiding it.

**7. Canonicalize paths and check symlink escape at operation time.**
Not just at index time. String prefix checks (`/safe/root` vs `/safe/root-evil`) are insufficient. A symlink to `~/.ssh` inside a cloned repo is not hypothetical.

**8. Unicode NFC/NFD normalization.**
macOS uses NFD. Without normalization the same file gets two identities. This is a correctness bug, not a locale feature.

---

## Writes

**9. Stale-version check immediately before commit.**
Read the hash used to plan the edit, re-read it right before writing, reject if changed. The author has the file open in their editor while the agent runs.

**10. Snapshot before mutating; every mutation declares a validator.**
Pre-image to `transactions/<txn_id>/` with its BLAKE3 recorded. A tool with no validator is `Irreversible + Unverifiable` and needs explicit confirmation. Exit code alone is never verification.

**11. Back up SQLite before any migration.**
`VACUUM INTO` a timestamped file. Derived indexes are disposable; **corrections and workspace policy are not**. Migration failure restores and refuses to start writes.

---

## Trust

**12. Retrieved content never grants authority.** ⚠️
File text, tool output, OCR text, transcripts, EXIF values, MCP tool descriptions, web content — all data, even when it contains instructions.

- Serialize into labelled blocks with runtime-generated delimiters ([Part 6 §114](../../../docs/Part_6_Engineering_Reference.md)). Never Markdown fences an attacker can close
- The system prompt is assembled by the runtime only, from templates in the binary
- Untrusted blocks are never last — they must not be the final instruction
- **The prompt is defence in depth. Enforcement is independent** — the policy layer blocks the action even if the model fully complies with injected text

**13. Agent-written content is marked `origin = SELF` and barred from evidence authority.**
Otherwise a summary the agent wrote into a watched folder gets re-indexed and cites itself back as independent corroboration. Searchable: yes. Able to support a claim: no.

**14. Authority class on every fact.**
`USER_ASSERTION` > `DETERMINISTIC_FACT` > `EXTRACTED_FACT` > `INFERRED_FACT` > `HYPOTHESIS`. A fact never loses its class. VLM captions are `HYPOTHESIS` and can never establish a fact on their own. Without this you can't tell knowledge from guessing, and you'll stop trusting the output.

---

## Availability

**15. Search works with no LLM, no GPU, no network.**
Lexical and metadata search are independently useful. They're the fallback while everything else is half-built.

---

## Review checklist

For any diff touching files, paths, parsing, evidence or mutations:

- [ ] Does every new IR node carry a `source_span`?
- [ ] Is anything keyed on a path instead of `file_id`?
- [ ] Does any code path open a file without checking placeholder flags?
- [ ] Are paths canonicalized at the point of the operation?
- [ ] Does a write re-check the version hash immediately before committing?
- [ ] Does the mutation have a snapshot and a validator?
- [ ] Does retrieved text reach a prompt outside a labelled block?
- [ ] Is agent-written output marked `SELF`?
- [ ] Does every persisted fact carry an authority class and evidence?
- [ ] Is a derived index doing something canonical state couldn't rebuild?
- [ ] Does this still work with the network off?

## Corresponding tests

Named tests, from [Part 6 §116.3](../../../docs/Part_6_Engineering_Reference.md). An invariant without a test is a comment.

```
no_fact_without_provenance      captions_cannot_be_facts
self_origin_excluded            every_mutation_has_validator
secrets_never_in_config         derived_rebuild_preserves_corrections
placeholder_never_hydrated      symlink_escape_blocked
stale_write_rejected            nfc_nfd_single_identity
```

Plus the adversarial corpus in `TRACKER.md` — green before any write tool ships, and it only ever grows.
